//! The blocker's worker: the one thread that touches the lists' files, runs the converter and
//! the fetch, and hands the result to the engine.
//!
//! Nothing here waits for a page: a list that is not ready blocks nothing and the browser works.
//! One list at a time moves through *fetch* (a `curl` child under its own label) and *activate*
//! (use what is on disk: ask the engine's store for the parts compiled last time, and only when
//! that fails convert and compile). The engine answers each part with
//! [`Cmd::ListResult`], which the window forwards. All steps are synchronous methods of
//! [`Worker`] so the host tests drive them with a fake runner and a recording engine.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use xui_webkit::abi::ListStatus;
use xui_webkit::engine::Engine;

use super::lists::{self, ListSpec, LISTS};
use super::update::{self, Meta};
use super::{Blocker, ListState};

/// What the worker is told.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// The window is up: use what is on disk, fetch what is due.
    Start,
    /// The user switched a list on or off.
    ListsChanged,
    /// Fetch now, whatever the schedule says ("Update now").
    UpdateNow,
    /// The engine's answer for one compiled part.
    ListResult { id: String, status: ListStatus },
    Stop,
}

/// How a fetch ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    /// What the program said on its error output, trimmed.
    pub note: String,
}

/// Runs `curl` (a child under the `listupdate` label on LazyOS).
pub trait Runner: Send + Sync {
    fn curl(&self, args: &[String]) -> io::Result<Exit>;
}

/// The worker's state. `now` is seconds since the epoch.
pub struct Worker {
    dir: PathBuf,
    engine: Arc<dyn Engine + Send + Sync>,
    runner: Arc<dyn Runner>,
    blocker: Blocker,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pending: HashMap<&'static str, Pending>,
}

/// The parts of one list the engine has been asked for.
struct Pending {
    parts: Vec<String>,
    ready: usize,
    /// Asked with `blocking_load` (the store may not have them) and not yet compiled from source.
    loading: bool,
    rules: usize,
    dropped: usize,
}

fn say(line: &str) {
    println!("WEBPLUS:BLOCK:{line}");
}

impl Worker {
    pub fn new(
        dir: PathBuf,
        engine: Arc<dyn Engine + Send + Sync>,
        runner: Arc<dyn Runner>,
        blocker: Blocker,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Worker {
        Worker {
            dir,
            engine,
            runner,
            blocker,
            now,
            pending: HashMap::new(),
        }
    }

    /// Runs the worker on a thread of its own; the sender is the way to it (the blocker keeps it,
    /// so the window's events reach the worker through [`Blocker::send`]).
    pub fn spawn(self) -> Sender<Cmd> {
        let (commands, inbox) = channel();
        self.blocker.connect(commands.clone());
        let spawned = std::thread::Builder::new()
            .name(String::from("blocker"))
            .spawn(move || {
                let mut worker = self;
                for command in inbox {
                    if !worker.handle(command) {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            say("FAIL:no thread");
        }
        commands
    }

    /// Does what `command` asks. False when the worker should end.
    pub fn handle(&mut self, command: Cmd) -> bool {
        match command {
            Cmd::Start => {
                let _ = fs::create_dir_all(&self.dir);
                for spec in &LISTS {
                    self.activate(spec);
                    self.fetch(spec, false);
                }
            }
            Cmd::ListsChanged => {
                for spec in &LISTS {
                    self.activate(spec);
                    self.fetch(spec, false);
                }
            }
            Cmd::UpdateNow => {
                for spec in &LISTS {
                    if self.enabled(spec) {
                        self.fetch(spec, true);
                    }
                }
            }
            Cmd::ListResult { id, status } => self.result(&id, status),
            Cmd::Stop => return false,
        }
        true
    }

    fn enabled(&self, spec: &ListSpec) -> bool {
        self.blocker.lists().iter().any(|l| l.id == spec.id && l.enabled)
    }

    fn set_state(&self, spec: &ListSpec, state: ListState) {
        self.blocker.update_list(spec.id, |l| l.state = state);
    }

    fn path(&self, spec: &ListSpec, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", spec.id))
    }

    fn read_meta(&self, spec: &ListSpec) -> Meta {
        fs::read_to_string(self.path(spec, "meta"))
            .map(|text| Meta::parse(&text))
            .unwrap_or_default()
    }

    fn write_meta(&self, spec: &ListSpec, meta: &Meta) {
        if let Err(e) = fs::write(self.path(spec, "meta"), meta.render()) {
            say(&format!("FAIL:{}:meta:{e}", spec.id));
        }
    }

    /// Uses the list's file on disk: switches it off, loads what the engine's store holds, or
    /// converts and compiles.
    fn activate(&mut self, spec: &'static ListSpec) {
        let meta = self.read_meta(spec);
        if !self.enabled(spec) {
            self.pending.remove(spec.id);
            for part in &meta.parts {
                self.engine.blocking_forget(part);
            }
            self.set_state(spec, ListState::Waiting);
            return;
        }
        let Ok(source) = fs::read(self.path(spec, "txt")) else {
            self.set_state(spec, ListState::Waiting);
            return;
        };
        self.blocker.update_list(spec.id, |l| {
            l.fetched = (meta.fetched > 0).then_some(meta.fetched);
        });
        if meta.hash == lists::source_hash(&source) && !meta.parts.is_empty() {
            self.set_state(spec, ListState::Compiling);
            self.pending.insert(
                spec.id,
                Pending {
                    parts: meta.parts.clone(),
                    ready: 0,
                    loading: true,
                    rules: meta.rules,
                    dropped: meta.dropped,
                },
            );
            for part in &meta.parts {
                self.engine.blocking_load(part);
            }
        } else {
            self.compile(spec, &source);
        }
    }

    /// Converts `source` and gives the parts to the engine; the old parts it no longer needs go.
    fn compile(&mut self, spec: &'static ListSpec, source: &[u8]) {
        let started = (self.now)();
        let prepared = lists::prepare(spec.id, source);
        let mut meta = self.read_meta(spec);
        let ids: Vec<String> = prepared.parts.iter().map(|(id, _)| id.clone()).collect();
        for old in meta.parts.iter().filter(|old| !ids.contains(old)) {
            self.engine.blocking_forget(old);
        }
        meta.hash = prepared.hash.clone();
        meta.parts = ids.clone();
        meta.rules = prepared.rules();
        meta.dropped = prepared.dropped();
        self.write_meta(spec, &meta);
        say(&format!(
            "CONVERT:{}:rules={}:dropped={}:parts={}:t={}",
            spec.id,
            meta.rules,
            meta.dropped,
            ids.len(),
            (self.now)().saturating_sub(started)
        ));
        if ids.is_empty() {
            self.pending.remove(spec.id);
            self.set_state(spec, ListState::Failed("the list has no rule the engine can use".into()));
            return;
        }
        self.set_state(spec, ListState::Compiling);
        self.pending.insert(
            spec.id,
            Pending {
                parts: ids,
                ready: 0,
                loading: false,
                rules: meta.rules,
                dropped: meta.dropped,
            },
        );
        for (id, json) in &prepared.parts {
            self.engine.blocking_compile(id, json);
        }
    }

    /// The engine's answer for part `id`.
    fn result(&mut self, id: &str, status: ListStatus) {
        let Some(spec) = LISTS.iter().find(|spec| {
            self.pending
                .get(spec.id)
                .is_some_and(|p| p.parts.iter().any(|part| part == id))
        }) else {
            return;
        };
        let loading = self.pending[spec.id].loading;
        match status {
            ListStatus::Ready => {
                let Some(p) = self.pending.get_mut(spec.id) else {
                    return;
                };
                p.ready += 1;
                if p.ready == p.parts.len() {
                    let (rules, dropped) = (p.rules, p.dropped);
                    self.blocker.update_list(spec.id, |l| {
                        l.state = ListState::Ready;
                        l.rules = rules;
                        l.dropped = dropped;
                    });
                    say(&format!("READY:{}:rules={rules}", spec.id));
                }
            }
            // The store has no such list (a new profile, a WebKit that changed its format):
            // convert the source and compile it, once.
            ListStatus::NotStored | ListStatus::Stale if loading => {
                say(&format!("RECOMPILE:{}:{}", spec.id, status.name()));
                match fs::read(self.path(spec, "txt")) {
                    Ok(source) => self.compile(spec, &source),
                    Err(_) => self.set_state(spec, ListState::Waiting),
                }
            }
            ListStatus::Unavailable => {
                self.pending.remove(spec.id);
                self.set_state(spec, ListState::Unavailable);
                say(&format!("UNAVAILABLE:{}", spec.id));
            }
            other => {
                self.pending.remove(spec.id);
                let why = format!("the engine did not take the list ({})", other.name());
                self.set_state(spec, ListState::Failed(why));
                say(&format!("FAIL:{}:{}", spec.id, other.name()));
            }
        }
    }

    /// Fetches the list when it is due (or `force`), keeping a good copy on any failure.
    fn fetch(&mut self, spec: &'static ListSpec, force: bool) {
        if !self.enabled(spec) {
            return;
        }
        let now = (self.now)();
        let mut meta = self.read_meta(spec);
        if !force && !meta.due(now) {
            return;
        }
        let was_ready = self
            .blocker
            .lists()
            .iter()
            .any(|l| l.id == spec.id && l.state == ListState::Ready);
        if !was_ready {
            self.set_state(spec, ListState::Fetching);
        }
        meta.attempted = now;
        self.write_meta(spec, &meta);

        let download = self.path(spec, "dl");
        let args = update::curl_args(spec.url, &download.to_string_lossy(), &meta);
        let outcome = match self.runner.curl(&args) {
            Ok(exit) if exit.code == 0 => fs::read(&download)
                .map_err(|e| format!("no answer: {e}"))
                .and_then(|raw| {
                    update::parse_reply(&raw).ok_or_else(|| String::from("not an HTTP answer"))
                }),
            Ok(exit) => Err(format!("curl exit {}: {}", exit.code, exit.note)),
            Err(e) => Err(format!("curl did not run: {e}")),
        };
        let _ = fs::remove_file(&download);
        match outcome {
            Ok(reply) if reply.status == 304 => {
                meta.fetched = now;
                self.write_meta(spec, &meta);
                self.blocker.update_list(spec.id, |l| l.fetched = Some(now));
                say(&format!("UPDATE:{}:NOTMODIFIED", spec.id));
                if !was_ready {
                    self.activate(spec);
                }
            }
            Ok(reply) if reply.status == 200 && update::looks_like_list(&reply.body) => {
                let text = String::from_utf8_lossy(&reply.body).into_owned();
                let staging = self.path(spec, "new");
                let stored = fs::write(&staging, &reply.body)
                    .and_then(|()| fs::rename(&staging, self.path(spec, "txt")));
                if let Err(e) = stored {
                    self.fetch_failed(spec, was_ready, &format!("cannot keep the list: {e}"));
                    return;
                }
                meta.fetched = now;
                meta.expires = update::expires_of(&text);
                meta.etag = reply.etag;
                meta.last_modified = reply.last_modified;
                self.write_meta(spec, &meta);
                say(&format!("UPDATE:{}:OK:bytes={}", spec.id, reply.body.len()));
                self.activate(spec);
            }
            Ok(reply) => {
                let why = if reply.status == 200 {
                    String::from("the answer is not a filter list")
                } else {
                    format!("HTTP {}", reply.status)
                };
                self.fetch_failed(spec, was_ready, &why);
            }
            Err(why) => self.fetch_failed(spec, was_ready, &why),
        }
    }

    /// A failed fetch changes nothing about a list that works.
    fn fetch_failed(&self, spec: &ListSpec, was_ready: bool, why: &str) {
        say(&format!("UPDATE:{}:FAIL:{}", spec.id, why.replace(['\n', '\r'], " ")));
        if !was_ready {
            self.set_state(spec, ListState::Failed(why.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use xui_webkit::engine::{Button, MouseKind};

    /// Records the blocking calls; the rest of the engine is not used here.
    #[derive(Default)]
    struct Recording {
        calls: Mutex<Vec<String>>,
    }

    impl Recording {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut self.calls.lock().unwrap())
        }
    }

    impl Engine for Recording {
        fn load(&self, _: &str) {}
        fn reload(&self) {}
        fn stop(&self) {}
        fn back(&self) {}
        fn forward(&self) {}
        fn resize(&self, _: i32, _: i32) {}
        fn mouse(&self, _: MouseKind, _: Button, _: i32, _: i32, _: u32) {}
        fn wheel(&self, _: i32, _: i32, _: i32, _: i32, _: u32) {}
        fn key(&self, _: bool, _: u32, _: &str, _: u32) {}
        fn focus(&self, _: bool) {}
        fn download(&self, _: &str) {}
        fn download_cancel(&self, _: u64) {}
        fn dialog_reply(&self, _: u64, _: bool, _: &str) {}
        fn blocking_load(&self, id: &str) {
            self.calls.lock().unwrap().push(format!("load {id}"));
        }
        fn blocking_compile(&self, id: &str, json: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("compile {id} {}", json.len()));
        }
        fn blocking_forget(&self, id: &str) {
            self.calls.lock().unwrap().push(format!("forget {id}"));
        }
    }

    /// A fake `curl`: writes what the test staged to the `-o` file and returns its exit.
    struct Fake {
        answer: Mutex<Option<io::Result<(Exit, Vec<u8>)>>>,
        seen: Mutex<Vec<Vec<String>>>,
    }

    impl Fake {
        fn answering(raw: &[u8]) -> Arc<Fake> {
            Arc::new(Fake {
                answer: Mutex::new(Some(Ok((
                    Exit {
                        code: 0,
                        note: String::new(),
                    },
                    raw.to_vec(),
                )))),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn failing(code: i32, note: &str) -> Arc<Fake> {
            Arc::new(Fake {
                answer: Mutex::new(Some(Ok((
                    Exit {
                        code,
                        note: note.into(),
                    },
                    Vec::new(),
                )))),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl Runner for Fake {
        fn curl(&self, args: &[String]) -> io::Result<Exit> {
            self.seen.lock().unwrap().push(args.to_vec());
            let (exit, raw) = self
                .answer
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Err(io::Error::other("asked twice")))?;
            let out = args.windows(2).find(|w| w[0] == "-o").map(|w| w[1].clone());
            if let (Some(out), 0) = (out, exit.code) {
                fs::write(out, raw)?;
            }
            Ok(exit)
        }
    }

    fn reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let headers = if headers.is_empty() {
            String::new()
        } else {
            format!("{headers}\r\n")
        };
        let mut raw = format!("HTTP/1.1 {status}\r\n{headers}\r\n").into_bytes();
        raw.extend_from_slice(body);
        raw
    }

    fn list_text() -> Vec<u8> {
        let mut text = b"[Adblock Plus 2.0]\n! Expires: 2 days\n||ads.example^\n@@||ads.example/ok\n".to_vec();
        text.extend(std::iter::repeat_n(b'!', 1_200));
        text.push(b'\n');
        text
    }

    struct Fixture {
        dir: PathBuf,
        engine: Arc<Recording>,
        blocker: Blocker,
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lazywebplus-blocker-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn worker(name: &str, runner: Arc<dyn Runner>, now: u64) -> (Worker, Fixture) {
        worker_in(dir(name), runner, now)
    }

    /// A worker over an existing folder: a later run of the browser finds the earlier one's files.
    fn worker_in(dir: PathBuf, runner: Arc<dyn Runner>, now: u64) -> (Worker, Fixture) {
        let engine = Arc::new(Recording::default());
        let blocker = Blocker::volatile();
        let w = Worker::new(
            dir.clone(),
            engine.clone(),
            runner,
            blocker.clone(),
            Arc::new(move || now),
        );
        (w, Fixture { dir, engine, blocker })
    }

    fn state(f: &Fixture) -> ListState {
        f.blocker.lists()[0].state.clone()
    }

    #[test]
    fn a_first_start_fetches_converts_compiles_and_becomes_ready() {
        let body = list_text();
        let fake = Fake::answering(&reply("200 OK", "ETag: \"v1\"", &body));
        let (mut w, f) = worker("first", fake.clone(), 1_791_000_000);
        assert!(w.handle(Cmd::Start));
        let calls = f.engine.take();
        let hash = lists::source_hash(&body);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with(&format!("compile easylist-{hash}-0 ")));
        assert_eq!(state(&f), ListState::Compiling);

        w.handle(Cmd::ListResult {
            id: format!("easylist-{hash}-0"),
            status: ListStatus::Ready,
        });
        let info = &f.blocker.lists()[0];
        assert_eq!(info.state, ListState::Ready);
        assert_eq!((info.rules, info.dropped), (2, 0));
        assert_eq!(info.fetched, Some(1_791_000_000));
        assert!(f.blocker.any_ready());

        let meta = Meta::parse(&fs::read_to_string(f.dir.join("easylist.meta")).unwrap());
        assert_eq!(meta.etag.as_deref(), Some("\"v1\""));
        assert_eq!(meta.expires, 2 * 86_400);
        assert_eq!(meta.parts, [format!("easylist-{hash}-0")]);
        assert!(!f.dir.join("easylist.dl").exists(), "the download is removed");
        assert_eq!(fs::read(f.dir.join("easylist.txt")).unwrap(), body);
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_later_start_loads_the_stored_parts_and_compiles_nothing() {
        let body = list_text();
        let fake = Fake::answering(&reply("200 OK", "", &body));
        let (mut w, f) = worker("later", fake, 1_791_000_000);
        w.handle(Cmd::Start);
        let hash = lists::source_hash(&body);
        let id = format!("easylist-{hash}-0");
        w.handle(Cmd::ListResult {
            id: id.clone(),
            status: ListStatus::Ready,
        });
        f.engine.take();

        // A second run an hour later: not due, so no fetch; the store has the part.
        let nothing = Fake::failing(7, "must not run");
        let (mut again, f2) = worker_in(f.dir.clone(), nothing.clone(), 1_791_003_600);
        again.handle(Cmd::Start);
        assert!(nothing.seen.lock().unwrap().is_empty(), "nothing was due");
        assert_eq!(f2.engine.take(), [format!("load {id}")]);
        again.handle(Cmd::ListResult {
            id,
            status: ListStatus::Ready,
        });
        assert_eq!(f2.blocker.lists()[0].state, ListState::Ready);
        assert_eq!(f2.blocker.lists()[0].rules, 2, "the counts came from the record");
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_part_the_store_lost_is_compiled_from_the_source_once() {
        let body = list_text();
        let fake = Fake::answering(&reply("200 OK", "", &body));
        let (mut w, f) = worker("lost", fake, 1_791_000_000);
        w.handle(Cmd::Start);
        let id = format!("easylist-{}-0", lists::source_hash(&body));
        w.handle(Cmd::ListResult {
            id: id.clone(),
            status: ListStatus::Ready,
        });
        f.engine.take();

        // Restart with an empty store: the load comes back NotStored.
        let (mut again, f2) = worker_in(f.dir.clone(), Fake::failing(7, ""), 1_791_000_100);
        again.handle(Cmd::Start);
        assert_eq!(f2.engine.take(), [format!("load {id}")]);
        again.handle(Cmd::ListResult {
            id: id.clone(),
            status: ListStatus::NotStored,
        });
        let calls = f2.engine.take();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with(&format!("compile {id} ")));
        // A second refusal is a failure, not a loop.
        again.handle(Cmd::ListResult {
            id,
            status: ListStatus::CompileFailed,
        });
        assert!(matches!(f2.blocker.lists()[0].state, ListState::Failed(_)));
        assert!(f2.engine.take().is_empty());
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_failed_or_broken_fetch_keeps_the_list_that_works() {
        let body = list_text();
        let (mut w, f) = worker("keep", Fake::answering(&reply("200 OK", "", &body)), 1_791_000_000);
        w.handle(Cmd::Start);
        let id = format!("easylist-{}-0", lists::source_hash(&body));
        w.handle(Cmd::ListResult {
            id,
            status: ListStatus::Ready,
        });
        f.engine.take();

        for fake in [
            Fake::failing(6, "Could not resolve host"),
            Fake::answering(&reply("200 OK", "", b"<html>captive portal</html>")),
            Fake::answering(&reply("503 Service Unavailable", "", b"")),
        ] {
            w.runner = fake;
            w.handle(Cmd::UpdateNow);
            assert_eq!(state(&f), ListState::Ready, "still blocking");
            assert!(f.engine.take().is_empty());
            assert_eq!(fs::read(f.dir.join("easylist.txt")).unwrap(), body);
        }
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_list_that_never_fetched_shows_why() {
        let (mut w, f) = worker("nofetch", Fake::failing(6, "Could not resolve host"), 5);
        w.handle(Cmd::Start);
        match state(&f) {
            ListState::Failed(why) => assert!(why.contains("Could not resolve host"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(!f.blocker.any_ready());
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_new_version_replaces_the_old_parts() {
        let body = list_text();
        let (mut w, f) = worker("new", Fake::answering(&reply("200 OK", "", &body)), 1_791_000_000);
        w.handle(Cmd::Start);
        let old = format!("easylist-{}-0", lists::source_hash(&body));
        w.handle(Cmd::ListResult {
            id: old.clone(),
            status: ListStatus::Ready,
        });
        f.engine.take();

        let mut newer = body.clone();
        newer.extend_from_slice(b"||tracker.example^\n");
        w.runner = Fake::answering(&reply("200 OK", "", &newer));
        w.handle(Cmd::UpdateNow);
        let calls = f.engine.take();
        let fresh = format!("easylist-{}-0", lists::source_hash(&newer));
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0], format!("forget {old}"));
        assert!(calls[1].starts_with(&format!("compile {fresh} ")));
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn switching_a_list_off_forgets_its_parts_and_on_loads_them_again() {
        let body = list_text();
        let (mut w, f) = worker("switch", Fake::answering(&reply("200 OK", "", &body)), 1_791_000_000);
        w.handle(Cmd::Start);
        let id = format!("easylist-{}-0", lists::source_hash(&body));
        w.handle(Cmd::ListResult {
            id: id.clone(),
            status: ListStatus::Ready,
        });
        f.engine.take();

        f.blocker.set_list_enabled("easylist", false);
        w.handle(Cmd::ListsChanged);
        assert_eq!(f.engine.take(), [format!("forget {id}")]);
        assert!(!f.blocker.any_ready());

        f.blocker.set_list_enabled("easylist", true);
        w.handle(Cmd::ListsChanged);
        // The record still names the same source, so the part is compiled again from it.
        let calls = f.engine.take();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn a_missing_store_is_reported_and_stops_the_list() {
        let body = list_text();
        let (mut w, f) = worker("nostore", Fake::answering(&reply("200 OK", "", &body)), 1_791_000_000);
        w.handle(Cmd::Start);
        let id = format!("easylist-{}-0", lists::source_hash(&body));
        w.handle(Cmd::ListResult {
            id,
            status: ListStatus::Unavailable,
        });
        assert_eq!(state(&f), ListState::Unavailable);
        assert!(!f.blocker.any_ready());
        let _ = fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn an_unknown_part_is_ignored_and_stop_ends_the_worker() {
        let (mut w, f) = worker("ignore", Fake::failing(7, ""), 1);
        w.handle(Cmd::ListResult {
            id: "someone-elses".into(),
            status: ListStatus::Ready,
        });
        assert!(matches!(state(&f), ListState::Waiting));
        assert!(!w.handle(Cmd::Stop));
        let _ = fs::remove_dir_all(&f.dir);
    }
}

//! The blocker's worker: the one thread that touches the lists' files, runs the converter and
//! the fetch, and hands the result to the engine.
//!
//! Nothing here waits for a page: a list that is not ready blocks nothing and the browser works.
//! One list at a time moves through *fetch* (a `curl` child under its own label) and *activate*
//! (use what is on disk: ask the engine's store for the parts compiled last time, and only when
//! that fails convert and compile). The engine answers each part with
//! [`Cmd::ListResult`], which the window forwards. All steps are synchronous methods of
//! [`Worker`] so the host tests drive them with a fake runner and a recording engine.

use std::collections::{HashMap, HashSet};
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
    ListResult {
        id: String,
        status: ListStatus,
    },
    /// A fetch that ran on a thread of its own ended: what it brought back, and the list's state
    /// when it began (a list that was working keeps working whatever the fetch says).
    Fetched {
        id: String,
        outcome: Result<update::Reply, String>,
        was_ready: bool,
        now: u64,
    },
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
    /// Fetches run on a thread of their own (the engine's answers must not wait behind a slow
    /// network): true once the worker is spawned; the tests run them in place.
    background: bool,
    /// Lists with a fetch under way.
    fetching: HashSet<&'static str>,
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
            background: false,
            fetching: HashSet::new(),
        }
    }

    /// Runs the worker on a thread of its own; the sender is the way to it (the blocker keeps it,
    /// so the window's events reach the worker through [`Blocker::send`]).
    pub fn spawn(mut self) -> Sender<Cmd> {
        self.background = true;
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
            Cmd::Fetched {
                id,
                outcome,
                was_ready,
                now,
            } => {
                if let Some(spec) = lists::spec(&id) {
                    self.fetching.remove(spec.id);
                    self.finish_fetch(spec, outcome, was_ready, now);
                }
            }
            Cmd::Stop => return false,
        }
        true
    }

    fn enabled(&self, spec: &ListSpec) -> bool {
        self.blocker
            .lists()
            .iter()
            .any(|l| l.id == spec.id && l.enabled)
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
            self.set_state(
                spec,
                ListState::Failed("the list has no rule the engine can use".into()),
            );
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

    /// Fetches the list when it is due (or `force`), keeping a good copy on any failure. The
    /// download runs on a thread of its own once the worker is spawned, so the engine's answers
    /// (and a list that is already blocking) never wait behind a slow network.
    fn fetch(&mut self, spec: &'static ListSpec, force: bool) {
        if !self.enabled(spec) || self.fetching.contains(spec.id) {
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
        let runner = Arc::clone(&self.runner);
        let work = move || -> Result<update::Reply, String> {
            let outcome = match runner.curl(&args) {
                Ok(exit) if exit.code == 0 => fs::read(&download)
                    .map_err(|e| format!("no answer: {e}"))
                    .and_then(|raw| {
                        update::parse_reply(&raw).ok_or_else(|| String::from("not an HTTP answer"))
                    }),
                Ok(exit) => Err(format!("curl exit {}: {}", exit.code, exit.note)),
                Err(e) => Err(format!("curl did not run: {e}")),
            };
            let _ = fs::remove_file(&download);
            outcome
        };
        if !self.background {
            let outcome = work();
            self.finish_fetch(spec, outcome, was_ready, now);
            return;
        }
        self.fetching.insert(spec.id);
        let blocker = self.blocker.clone();
        let id = spec.id.to_owned();
        let spawned = std::thread::Builder::new()
            .name(String::from("blocker-fetch"))
            .spawn(move || {
                let outcome = work();
                blocker.send(Cmd::Fetched {
                    id,
                    outcome,
                    was_ready,
                    now,
                });
            });
        if spawned.is_err() {
            self.fetching.remove(spec.id);
            self.fetch_failed(spec, was_ready, "no thread to fetch with");
        }
    }

    /// What a fetch brought back, applied: the list kept, replaced or left alone.
    fn finish_fetch(
        &mut self,
        spec: &'static ListSpec,
        outcome: Result<update::Reply, String>,
        was_ready: bool,
        now: u64,
    ) {
        let mut meta = self.read_meta(spec);
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
        say(&format!(
            "UPDATE:{}:FAIL:{}",
            spec.id,
            why.replace(['\n', '\r'], " ")
        ));
        if !was_ready {
            self.set_state(spec, ListState::Failed(why.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests;

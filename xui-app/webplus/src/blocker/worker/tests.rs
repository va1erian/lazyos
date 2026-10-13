//! The worker's steps, driven with a fake `curl` and a recording engine.
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
    let mut text =
        b"[Adblock Plus 2.0]\n! Expires: 2 days\n||ads.example^\n@@||ads.example/ok\n".to_vec();
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
    let dir =
        std::env::temp_dir().join(format!("lazywebplus-blocker-{name}-{}", std::process::id()));
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
    (
        w,
        Fixture {
            dir,
            engine,
            blocker,
        },
    )
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
    assert!(
        !f.dir.join("easylist.dl").exists(),
        "the download is removed"
    );
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
    assert_eq!(
        f2.blocker.lists()[0].rules,
        2,
        "the counts came from the record"
    );
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
    let (mut w, f) = worker(
        "keep",
        Fake::answering(&reply("200 OK", "", &body)),
        1_791_000_000,
    );
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
    let (mut w, f) = worker(
        "new",
        Fake::answering(&reply("200 OK", "", &body)),
        1_791_000_000,
    );
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
    let (mut w, f) = worker(
        "switch",
        Fake::answering(&reply("200 OK", "", &body)),
        1_791_000_000,
    );
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
    let (mut w, f) = worker(
        "nostore",
        Fake::answering(&reply("200 OK", "", &body)),
        1_791_000_000,
    );
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

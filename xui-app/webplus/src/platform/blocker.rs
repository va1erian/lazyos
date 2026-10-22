//! The ad blocker's platform side: its settings in `confd` and the `curl` child that fetches the
//! lists (webd decides each navigation from the switches the window gives it).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;

/// `confd`'s registered name: a store call blocks while it is not registered, so nothing is read or
/// written unless it already is.
const CONFD: &str = "os.lazy.confd";

/// The ad blocker's switches, as `confd` keys under `user/<uid>/lazywebplus/blocker/`: the global
/// switch (a bool), the sites left alone and the lists turned off (space separated text). They are
/// the user's, out of reach of the engine's processes, which may write the profile folder.
pub(super) struct ConfdSettings {
    store: ConfdStore,
    uid: u32,
}

impl ConfdSettings {
    fn key(&self, name: &str) -> String {
        format!("user/{}/lazywebplus/blocker/{name}", self.uid)
    }

    fn text(&self, name: &str) -> Option<String> {
        match self.store.get(&self.key(name))? {
            StoreValue::Str(text) => Some(text),
            StoreValue::Bool(on) => Some(on.to_string()),
            _ => None,
        }
    }
}

impl Persist for ConfdSettings {
    fn load(&self) -> Settings {
        Settings::from_texts(
            self.text("enabled").as_deref(),
            &self.text("exceptions").unwrap_or_default(),
            &self.text("lists_off").unwrap_or_default(),
        )
    }

    fn save(&self, settings: &Settings) {
        let writes = [
            ("enabled", StoreValue::Bool(settings.enabled)),
            ("exceptions", StoreValue::Str(settings.exceptions_text())),
            ("lists_off", StoreValue::Str(settings.lists_off_text())),
        ];
        for (name, value) in writes {
            if let Err(e) = self.store.set(&self.key(name), value) {
                crate::marker(&format!("BLOCK:SETTINGS:FAIL:{name}:{e}"));
            }
        }
    }
}

/// `confd` when it is up and tells this task its uid; settings kept nowhere otherwise.
pub(super) fn settings_store() -> Arc<dyn Persist> {
    if Service::try_connect(CONFD).is_none() {
        return Arc::new(Volatile);
    }
    let store = ConfdStore::new();
    match store.uid() {
        Some(uid) => Arc::new(ConfdSettings { store, uid }),
        None => Arc::new(Volatile),
    }
}

/// The longest one `curl` run may take; a hung transfer is killed, not waited for.
const CURL_DEADLINE: Duration = Duration::from_secs(600);
/// How often a running `curl` is looked at (for its end, the deadline and the quit).
const CURL_POLL: Duration = Duration::from_millis(50);
/// How long the quit waits for a `curl` it has killed to be collected.
const CURL_QUIT_WAIT: Duration = Duration::from_secs(3);

/// The window is closing: no new `curl`, and a running one is killed.
static CURL_STOP: AtomicBool = AtomicBool::new(false);
/// The `curl` children running now.
static CURL_RUNNING: AtomicU32 = AtomicU32::new(0);

/// Counts one running child for as long as it lives.
struct Running;

impl Running {
    fn new() -> Running {
        CURL_RUNNING.fetch_add(1, Ordering::AcqRel);
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        CURL_RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs `curl` as a child of this program: the kernel enters it under the manifest's
/// `[processes.listupdate]` label, which holds the network and the lists folder and nothing else.
/// The child is watched, not waited for: it is killed at its deadline and when the window closes
/// (`stop_children`), so quitting never leaves one behind (issue #862).
pub(super) struct CurlRunner;

impl Runner for CurlRunner {
    fn curl(&self, args: &[String]) -> std::io::Result<Exit> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        // Counted before the quit is looked at: `stop_children` sets the flag and then waits for the
        // count, so a run that starts after the flag still ends it (the loop below kills it).
        let _running = Running::new();
        if CURL_STOP.load(Ordering::Acquire) {
            return Ok(Exit {
                code: -1,
                note: String::from("the window is closing"),
            });
        }
        let mut child = Command::new(fhs::bin::CURL)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        // Drained on a thread of its own, so a chatty child cannot fill the pipe and stall.
        let stderr = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut text = Vec::new();
                let _ = pipe.by_ref().take(64 * 1024).read_to_end(&mut text);
                text
            })
        });
        let began = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if CURL_STOP.load(Ordering::Acquire) || began.elapsed() > CURL_DEADLINE {
                let _ = child.kill();
                break child.wait()?;
            }
            std::thread::sleep(CURL_POLL);
        };
        let text = stderr
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let note = String::from_utf8_lossy(&text)
            .trim()
            .chars()
            .take(200)
            .collect();
        Ok(Exit {
            code: status.code().unwrap_or(-1),
            note,
        })
    }
}

/// The window is closing: stops the list updater's `curl` (none starts afterwards) and waits for
/// it to be collected. True when none is left.
pub(super) fn stop_children() -> bool {
    CURL_STOP.store(true, Ordering::Release);
    let until = Instant::now() + CURL_QUIT_WAIT;
    while CURL_RUNNING.load(Ordering::Acquire) > 0 && Instant::now() < until {
        std::thread::sleep(CURL_POLL);
    }
    CURL_RUNNING.load(Ordering::Acquire) == 0
}

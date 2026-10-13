//! The ad blocker's side of the UI process: its settings in `confd`, the `curl` child, and the
//! engine's per-navigation question.

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

/// Runs `curl` as a child of this program: the kernel enters it under the manifest's
/// `[processes.listupdate]` label, which holds the network and the lists folder and nothing else.
pub(super) struct CurlRunner;

impl Runner for CurlRunner {
    fn curl(&self, args: &[String]) -> std::io::Result<Exit> {
        let output = std::process::Command::new(fhs::bin::CURL)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()?;
        let note = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(200)
            .collect();
        Ok(Exit {
            code: output.status.code().unwrap_or(-1),
            note,
        })
    }
}

/// Whether the rule lists apply to a page whose main-frame address is `url`: the user's switch,
/// their exceptions, a one-shot "reload without blocking" (`blocker::Blocker::decide`).
///
/// # Safety
/// `url` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_blocking(url: *const c_char) -> c_int {
    // SAFETY: by this function's contract.
    let url = unsafe { text(url) };
    c_int::from(shared().blocker.decide(&url))
}

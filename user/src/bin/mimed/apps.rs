//! `mimed`'s open-with registry: `(mime, verb)` to app id, plus the boot-time
//! defaults seeded for the built-in types.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Open-with defaults seeded at boot: `(mime, app, verbs)`. The app ids are
/// registry ids (`editor` -> `XEDITOR.ELF`, `paint`, `files`, `docs`); an app the image
/// does not ship falls back to the launch event alone.
const DEFAULT_APPS: &[(&str, &str, &[&str])] = &[
    ("text/plain", "editor", &["open", "edit"]),
    ("text/plain", "files", &["reveal"]),
    ("text/markdown", "editor", &["open", "edit"]),
    // The Docs app (litehtml) renders Markdown. It is shipped only when the
    // build had the zig toolchain, so it is a `view` verb rather than the
    // default `open`: an image without it still opens `.md` files in the Editor.
    ("text/markdown", "docs", &["view"]),
    ("text/x-rust", "editor", &["open", "edit"]),
    ("text/x-shellscript", "editor", &["open", "edit"]),
    ("image/png", "paint", &["open", "edit"]),
    ("image/png", "files", &["reveal"]),
    ("application/x-elf", "runner", &["open"]),
    ("application/octet-stream", "files", &["reveal"]),
];

/// One open-with registration.
struct Registration {
    mime: String,
    verb: String,
    app: String,
}

/// The open-with registry: `(mime, verb)` to app. Re-registering a pair
/// replaces its app, so a later policy wins.
pub(crate) struct AppRegistry {
    entries: Vec<Registration>,
}

impl AppRegistry {
    pub(crate) fn new() -> AppRegistry {
        AppRegistry {
            entries: Vec::new(),
        }
    }

    /// Add or replace the app for `(mime, verb)`.
    pub(crate) fn register(&mut self, mime_type: &str, app: &str, verb: &str) {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        let app = app.trim();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
        {
            entry.app = app.to_string();
        } else {
            self.entries.push(Registration {
                mime: mime_type,
                verb,
                app: app.to_string(),
            });
        }
    }

    /// The app registered for a type and verb (both case-insensitive).
    pub(crate) fn lookup(&self, mime_type: &str, verb: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                entry.mime.eq_ignore_ascii_case(mime_type.trim())
                    && entry.verb.eq_ignore_ascii_case(verb.trim())
            })
            .map(|entry| entry.app.as_str())
    }

    /// The verbs registered for a type, in registration order.
    pub(crate) fn verbs(&self, mime_type: &str) -> Vec<String> {
        let mut verbs: Vec<String> = Vec::new();
        for entry in &self.entries {
            if entry.mime.eq_ignore_ascii_case(mime_type.trim()) && !verbs.contains(&entry.verb) {
                verbs.push(entry.verb.clone());
            }
        }
        verbs
    }
}

/// Seed the open-with defaults.
pub(crate) fn seed_default_apps(apps: &mut AppRegistry) {
    for (mime_type, app, verbs) in DEFAULT_APPS {
        for verb in *verbs {
            apps.register(mime_type, app, verb);
        }
    }
}

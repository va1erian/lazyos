//! `mimed`'s open-with registry: `(mime, verb)` to app id, plus the boot-time
//! defaults seeded for the built-in types.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Open-with defaults seeded at boot: `(mime, app, verbs)`. The desktop apps
/// are core packages named by their `system_name` (issue #509), and each
/// registers the same verbs through `pkgd` from its manifest, so this table
/// only breaks ties; the Installer and `runner` are built-in registry ids. An
/// app the image does not ship falls back to the launch event alone.
const DEFAULT_APPS: &[(&str, &str, &[&str])] = &[
    ("text/plain", "os.lazy.editor", &["open", "edit"]),
    // `reveal`: Files opens the folder holding the path with the item
    // selected (issue #488).
    ("text/plain", "os.lazy.files", &["reveal"]),
    // Markdown opens in the Docs renderer, which is zig-built and therefore
    // optional; `edit` stays with the Editor so the file remains editable, and
    // [`DEFAULT_FALLBACKS`] names the Editor for `open` when Docs is absent.
    ("text/markdown", "os.lazy.docs", &["open", "view"]),
    ("text/markdown", "os.lazy.editor", &["edit"]),
    ("text/x-rust", "os.lazy.editor", &["open", "edit"]),
    ("text/x-shellscript", "os.lazy.editor", &["open", "edit"]),
    ("image/png", "os.lazy.paint", &["open", "edit"]),
    ("image/png", "os.lazy.files", &["reveal"]),
    // LazyWriter documents (issue #533).
    (
        "application/x-lazywriter",
        "os.lazy.writer",
        &["open", "edit"],
    ),
    // Archives (docs/archiver-plan.md).
    ("application/zip", "os.lazy.archiver", &["open"]),
    ("application/x-tar", "os.lazy.archiver", &["open"]),
    ("application/gzip", "os.lazy.archiver", &["open"]),
    ("application/x-xz", "os.lazy.archiver", &["open"]),
    ("application/zstd", "os.lazy.archiver", &["open"]),
    ("application/x-7z-compressed", "os.lazy.archiver", &["open"]),
    // PDF documents (docs/pdf-reader-plan.md).
    ("application/pdf", "os.lazy.pdf", &["open", "view"]),
    ("application/x-elf", "runner", &["open"]),
    (
        "application/x-lazyos-package",
        "installer",
        &["open", "install"],
    ),
    // An IDE's request to run a project under its own permissions (issue
    // #529): the Installer's development consent, `init`'s
    // `installer-develop` row.
    (
        "application/x-lazyos-package",
        "installer-develop",
        &["develop"],
    ),
    ("application/octet-stream", "os.lazy.files", &["reveal"]),
];

/// Fallback apps for a `(mime, verb)` whose primary the image does not ship:
/// `(mime, verb, app)`. The open path swaps to the fallback when `init` reports
/// the primary's ELF is not installed (see `handlers::open_path`).
const DEFAULT_FALLBACKS: &[(&str, &str, &str)] = &[("text/markdown", "open", "os.lazy.editor")];

/// One open-with registration: the app for `(mime, verb)`, plus an optional
/// fallback to use when the primary's ELF is not shipped.
struct Registration {
    mime: String,
    verb: String,
    app: String,
    fallback: Option<String>,
    /// Registrations this one replaced, oldest first, so withdrawing the
    /// replacing app (an installed app being removed) gives the type back to
    /// the handler it took it from instead of leaving it with none.
    shadowed: Vec<(String, Option<String>)>,
}

/// Most replaced registrations remembered per `(mime, verb)`.
const MAX_SHADOWED: usize = 8;

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

    /// Add or replace the app for `(mime, verb)`. Any fallback the pair had is
    /// cleared: the new policy has not named one.
    pub(crate) fn register(&mut self, mime_type: &str, app: &str, verb: &str) {
        self.register_with_fallback(mime_type, app, verb, None);
    }

    /// [`register`](Self::register), naming the app to use when `app` is not
    /// shipped.
    fn register_with_fallback(
        &mut self,
        mime_type: &str,
        app: &str,
        verb: &str,
        fallback: Option<&str>,
    ) {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        let app = app.trim();
        let fallback = fallback.map(str::trim);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
        {
            if entry.app != app {
                let replaced = (core::mem::take(&mut entry.app), entry.fallback.take());
                entry.shadowed.retain(|(shadowed, _)| shadowed != app);
                entry.shadowed.push(replaced);
                if entry.shadowed.len() > MAX_SHADOWED {
                    entry.shadowed.remove(0);
                }
            }
            entry.app = app.to_string();
            entry.fallback = fallback.map(str::to_string);
        } else {
            self.entries.push(Registration {
                mime: mime_type,
                verb,
                app: app.to_string(),
                fallback: fallback.map(str::to_string),
                shadowed: Vec::new(),
            });
        }
    }

    /// Withdraw `app`'s registration for `(mime, verb)`. If `app` holds it, the
    /// registration it replaced (if any) takes over again, else the pair is
    /// dropped; if another app holds it, only `app`'s shadowed claim goes, so a
    /// later registration is never undone. Returns whether anything changed.
    pub(crate) fn unregister(&mut self, mime_type: &str, app: &str, verb: &str) -> bool {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        let app = app.trim();
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.mime == mime_type && entry.verb == verb)
        else {
            return false;
        };
        let entry = &mut self.entries[index];
        if entry.app == app {
            match entry.shadowed.pop() {
                Some((previous, fallback)) => {
                    entry.app = previous;
                    entry.fallback = fallback;
                }
                None => {
                    self.entries.remove(index);
                }
            }
            true
        } else {
            let before = entry.shadowed.len();
            entry.shadowed.retain(|(shadowed, _)| shadowed != app);
            entry.shadowed.len() != before
        }
    }

    /// Name the fallback for an already-registered `(mime, verb)`; a no-op for
    /// a pair that was never registered.
    fn set_fallback(&mut self, mime_type: &str, verb: &str, fallback: &str) {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
        {
            entry.fallback = Some(fallback.trim().to_string());
        }
    }

    /// The app registered for a type and verb (both case-insensitive).
    pub(crate) fn lookup(&self, mime_type: &str, verb: &str) -> Option<&str> {
        self.resolve(mime_type, verb).map(|(app, _)| app)
    }

    /// The app registered for a type and verb, plus its fallback app when the
    /// registration names one. Both keys are matched case-insensitively.
    pub(crate) fn resolve(&self, mime_type: &str, verb: &str) -> Option<(&str, Option<&str>)> {
        self.entries
            .iter()
            .find(|entry| {
                entry.mime.eq_ignore_ascii_case(mime_type.trim())
                    && entry.verb.eq_ignore_ascii_case(verb.trim())
            })
            .map(|entry| (entry.app.as_str(), entry.fallback.as_deref()))
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

/// The app to use given whether the primary is shipped: the fallback when the
/// primary is not, else the primary. Pure, so the fallback policy is testable
/// without a running `init`.
pub(crate) fn choose<'a>(
    primary: &'a str,
    fallback: Option<&'a str>,
    primary_shipped: bool,
) -> &'a str {
    match fallback {
        Some(fallback) if !primary_shipped => fallback,
        _ => primary,
    }
}

/// Seed the open-with defaults and their fallbacks.
pub(crate) fn seed_default_apps(apps: &mut AppRegistry) {
    for (mime_type, app, verbs) in DEFAULT_APPS {
        for verb in *verbs {
            apps.register(mime_type, app, verb);
        }
    }
    for (mime_type, verb, fallback) in DEFAULT_FALLBACKS {
        apps.set_fallback(mime_type, verb, fallback);
    }
}

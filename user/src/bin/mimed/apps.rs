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
    // JPEG and BMP: the Picture Viewer opens, Paint edits. The viewer is an
    // optional package (`LAZYOS_PICTURES=1`), so [`DEFAULT_FALLBACKS`] names
    // Paint for `open` when the image does not ship it.
    ("image/jpeg", "os.lazy.pictures", &["open", "view"]),
    ("image/jpeg", "os.lazy.paint", &["edit"]),
    ("image/bmp", "os.lazy.pictures", &["open", "view"]),
    ("image/bmp", "os.lazy.paint", &["edit"]),
    ("image/openraster", "os.lazy.paint", &["open", "edit"]),
    ("image/openraster", "os.lazy.files", &["reveal"]),
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
    // Video (docs/video-player-plan.md).
    ("video/x-msvideo", "os.lazy.video", &["open", "view"]),
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
const DEFAULT_FALLBACKS: &[(&str, &str, &str)] = &[
    ("text/markdown", "open", "os.lazy.editor"),
    ("image/jpeg", "open", "os.lazy.paint"),
    ("image/bmp", "open", "os.lazy.paint"),
];

/// One app's claim on a `(mime, verb)`: the app, the app to use when it is not
/// shipped, and whose claim it is.
struct Claim {
    app: String,
    fallback: Option<String>,
    /// The uid that installed the app; [`MACHINE_WIDE`] for the image's own
    /// and for what an administrator installed for everyone. Only that user's
    /// `Open` sees a claim of one user (audit H10: a user's package must not
    /// take over a type for the next user who opens a file).
    owner: u32,
}

/// The owner of a claim everyone sees.
pub(crate) const MACHINE_WIDE: u32 = 0;

impl Claim {
    /// Whether `uid` (`None`: the service's own self-test, which sees all)
    /// may be sent to this claim's app.
    fn visible_to(&self, uid: Option<u32>) -> bool {
        self.owner == MACHINE_WIDE || uid.is_none_or(|uid| uid == self.owner)
    }
}

/// One open-with registration: the claims on `(mime, verb)`, the current one
/// last.
struct Registration {
    mime: String,
    verb: String,
    /// Never empty. Earlier claims are the registrations the later ones
    /// replaced, oldest first, so withdrawing the replacing app (an installed
    /// app being removed) gives the type back to the handler it took it from
    /// instead of leaving it with none.
    claims: Vec<Claim>,
}

/// Most claims remembered per `(mime, verb)` besides the current one.
const MAX_SHADOWED: usize = 8;

/// Most `(mime, verb)` pairs the registry holds. The user allocator never
/// gives memory back, so an unbounded table is a heap that only grows; the
/// image's seeded defaults and every installed app's handlers fit with room to
/// spare.
pub(crate) const MAX_REGISTRATIONS: usize = 1024;

/// The open-with registry: `(mime, verb)` to app. Re-registering a pair
/// replaces its app, so a later policy wins.
pub(crate) struct AppRegistry {
    entries: Vec<Registration>,
    /// Most pairs held ([`MAX_REGISTRATIONS`]; smaller in the self-test).
    limit: usize,
}

impl AppRegistry {
    pub(crate) fn new() -> AppRegistry {
        AppRegistry::with_limit(MAX_REGISTRATIONS)
    }

    /// A registry that refuses a `limit + 1`-th pair.
    pub(crate) fn with_limit(limit: usize) -> AppRegistry {
        AppRegistry {
            entries: Vec::new(),
            limit,
        }
    }

    /// Add or replace the machine-wide app for `(mime, verb)`. Any fallback
    /// the pair had is cleared: the new policy has not named one. `false` when
    /// the registry is full and the pair is new; replacing a held pair always
    /// succeeds.
    pub(crate) fn register(&mut self, mime_type: &str, app: &str, verb: &str) -> bool {
        self.register_owned(mime_type, app, verb, MACHINE_WIDE)
    }

    /// [`register`](Self::register) for an app `owner` installed.
    pub(crate) fn register_owned(
        &mut self,
        mime_type: &str,
        app: &str,
        verb: &str,
        owner: u32,
    ) -> bool {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        let claim = Claim {
            app: app.trim().to_string(),
            fallback: None,
            owner,
        };
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
        {
            // The app's own earlier claim is replaced, wherever it sits.
            entry.claims.retain(|held| held.app != claim.app);
            entry.claims.push(claim);
            if entry.claims.len() > MAX_SHADOWED + 1 {
                entry.claims.remove(0);
            }
        } else if self.entries.len() < self.limit {
            self.entries.push(Registration {
                mime: mime_type,
                verb,
                claims: alloc::vec![claim],
            });
        } else {
            return false;
        }
        true
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
        let before = entry.claims.len();
        entry.claims.retain(|held| held.app != app);
        if entry.claims.is_empty() {
            self.entries.remove(index);
            return true;
        }
        entry.claims.len() != before
    }

    /// Whether two apps `uid` may be sent to handle `(mime, verb)`: the only
    /// case in which a user's choice of app matters, so the only one that
    /// costs the `confd` lookup of it (`mimed` serves every open from one thread).
    pub(crate) fn contested(&self, mime_type: &str, verb: &str, uid: Option<u32>) -> bool {
        self.entry(mime_type, verb).is_some_and(|entry| {
            entry
                .claims
                .iter()
                .filter(|claim| claim.visible_to(uid))
                .count()
                > 1
        })
    }

    /// Name the fallback for an already-registered `(mime, verb)`; a no-op for
    /// a pair that was never registered.
    fn set_fallback(&mut self, mime_type: &str, verb: &str, fallback: &str) {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        let verb = verb.trim().to_ascii_lowercase();
        if let Some(claim) = self
            .entries
            .iter_mut()
            .find(|entry| entry.mime == mime_type && entry.verb == verb)
            .and_then(|entry| entry.claims.last_mut())
        {
            claim.fallback = Some(fallback.trim().to_string());
        }
    }

    /// The app registered for a type and verb (both case-insensitive).
    pub(crate) fn lookup(&self, mime_type: &str, verb: &str) -> Option<&str> {
        self.resolve(mime_type, verb).map(|(app, _)| app)
    }

    /// The app registered for a type and verb, plus its fallback app when the
    /// registration names one. Both keys are matched case-insensitively.
    pub(crate) fn resolve(&self, mime_type: &str, verb: &str) -> Option<(&str, Option<&str>)> {
        let claim = self.entry(mime_type, verb)?.claims.last()?;
        Some((claim.app.as_str(), claim.fallback.as_deref()))
    }

    /// [`resolve`](Self::resolve) for the user `uid` with a preference: only
    /// claims `uid` may be sent to count (the machine's, and its own).
    /// `preferred`, when that app holds a visible claim for the pair (it is
    /// the registered app, or one it replaced: installed, and handling the
    /// type), else the newest visible claim. An app that does not handle the
    /// type is never chosen, whatever the preference says.
    pub(crate) fn resolve_preferred(
        &self,
        mime_type: &str,
        verb: &str,
        preferred: Option<&str>,
        uid: Option<u32>,
    ) -> Option<(&str, Option<&str>)> {
        let entry = self.entry(mime_type, verb)?;
        let preferred = preferred.map(str::trim).filter(|app| !app.is_empty());
        let mut visible = entry.claims.iter().rev().filter(|c| c.visible_to(uid));
        let newest = visible.next()?;
        let chosen = preferred
            .and_then(|app| {
                core::iter::once(newest)
                    .chain(visible)
                    .find(|claim| claim.app == app)
            })
            .unwrap_or(newest);
        Some((chosen.app.as_str(), chosen.fallback.as_deref()))
    }

    fn entry(&self, mime_type: &str, verb: &str) -> Option<&Registration> {
        self.entries.iter().find(|entry| {
            entry.mime.eq_ignore_ascii_case(mime_type.trim())
                && entry.verb.eq_ignore_ascii_case(verb.trim())
        })
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
            let _ = apps.register(mime_type, app, verb);
        }
    }
    for (mime_type, verb, fallback) in DEFAULT_FALLBACKS {
        apps.set_fallback(mime_type, verb, fallback);
    }
}

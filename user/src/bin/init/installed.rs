//! The apps the package manager installed (`docs/packages.md`): since F5
//! (issue #509) the source of every desktop app, the core packages the image
//! ships included. `ListApps` reports them after the built-ins, `Launch`
//! resolves them (a bare `<short>` id is an alias of `os.lazy.<short>`, so
//! menus and launchers saved before F5 keep working), and the autostart opens
//! the ones whose manifest asks for it.
//!
//! The source of truth is `confd`: `pkgd` records one generated `Installed`
//! record per app under `sys/apps/<system_name>` (`idl/pkgd.midl`). `init`
//! reads that subtree itself rather than asking `pkgd`, on purpose: `pkgd`'s
//! `Remove` calls `init.Stop`, and `init` is one task serving every request, so
//! an `init` that blocked on `pkgd` while `pkgd` blocked on `init` would
//! deadlock both. `confd` never calls `init`, so that cycle cannot form. The
//! list is re-read on every `ListApps` and every launch of a non-built-in id,
//! which is what "re-read when an app is installed or removed" amounts to
//! without a subscription (a change is visible on the very next request).
//!
//! Every call carries a deadline, so a wedged `confd` costs a request half a
//! second, never the supervisor.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use confd::Value;
use deskmenu::hidden::{self, Hidden};
use user::messenger::confd::Client;
use user::messenger::pkgd::wire as pkgd;
use user::messenger::{registry, services, Endpoint};
use user::sys;

use super::state::Restart;

/// The `confd` subtree `pkgd` writes (`pkgstore::layout::CONFD_PREFIX`).
const PREFIX: &str = "sys/apps";
/// The namespace of the core apps' `system_name`s: `os.lazy.<short>`.
const CORE_PREFIX: &str = "os.lazy.";
/// Ticks one `confd` call may take (100 Hz): half a second.
const CALL_TICKS: u64 = 50;
/// Most installed apps `init` tracks, so a corrupt subtree cannot grow it
/// without bound.
const MAX_APPS: usize = 64;
/// Most hidden-app keys read per layer for one `ListApps`.
const MAX_HIDDEN_KEYS: usize = 64;

/// One installed app, ready to launch.
pub(super) struct InstalledApp {
    /// The `system_name`: the app id `Launch` and the menu use.
    pub(super) id: &'static str,
    /// Display name from the manifest.
    pub(super) name: String,
    /// `/apps/<install_dir>/<binary>`.
    pub(super) path: &'static str,
    /// The kernel policy label, `app:<system_name>`.
    pub(super) label: &'static str,
    /// The manifest's fixed arguments, one `argv` item each.
    pub(super) args: Vec<String>,
    /// Whether it runs under the Linux ABI personality.
    pub(super) linux: bool,
    /// The restart policy every launch of it gets.
    pub(super) restart: Restart,
    /// Whether the image ships it (`Origin::Core`).
    pub(super) core: bool,
    /// The menu group (`lazypkg::Category`).
    pub(super) category: String,
    /// Whether it opens when a session starts.
    pub(super) autostart: bool,
    /// The manifest's MIME verbs.
    pub(super) verbs: Vec<String>,
    /// Whether it is a resident app (`entry.resident`): no window needed,
    /// one instance per session, always in the tray.
    pub(super) resident: bool,
    /// Its 32-pixel icon (`fhs::icon_path`), for the desktop launchers.
    pub(super) icon: String,
}

/// The installed apps as of the last refresh.
pub(super) struct InstalledApps {
    apps: Vec<InstalledApp>,
    endpoint: Option<Endpoint>,
    /// Leaked strings by content: a service row holds `&'static str`s, and the
    /// user heap never frees them anyway, so each distinct string is leaked
    /// once and reused across refreshes.
    interned: Vec<&'static str>,
}

impl InstalledApps {
    pub(super) const fn new() -> InstalledApps {
        InstalledApps {
            apps: Vec::new(),
            endpoint: None,
            interned: Vec::new(),
        }
    }

    /// The installed app with this id, or with the core id it is a short
    /// alias of (`editor` -> `os.lazy.editor`).
    pub(super) fn find(&self, id: &str) -> Option<&InstalledApp> {
        let id = id.trim();
        self.apps
            .iter()
            .find(|app| app.id == id)
            .or_else(|| self.apps.iter().find(|app| alias_of(app.id) == Some(id)))
    }

    /// Every installed app, core first, then by `system_name`.
    pub(super) fn apps(&self) -> &[InstalledApp] {
        &self.apps
    }

    /// The `ListApps` reply: the `builtin` rows, then the installed apps,
    /// with `hidden` set for the caller `uid` on both (a built-in such as the
    /// Terminal can be hidden from the menu too).
    pub(super) fn infos(
        &mut self,
        mut builtin: Vec<services::AppInfo>,
        uid: u32,
    ) -> Vec<services::AppInfo> {
        let hidden = self.hidden(uid);
        for app in &mut builtin {
            app.hidden = hidden.hides(&app.id);
        }
        let installed = self.apps.iter().map(|app| services::AppInfo {
            id: app.id.to_string(),
            name: app.name.clone(),
            path: app.path.to_string(),
            restart: app.restart.label().to_string(),
            verbs: app.verbs.clone(),
            installed: true,
            origin: String::from(if app.core { "core" } else { "user" }),
            category: app.category.clone(),
            hidden: hidden.hides(app.id),
            autostart: app.autostart,
            icon: app.icon.clone(),
            resident: app.resident,
        });
        builtin.extend(installed);
        builtin
    }

    /// The ids that open when a session starts: core apps first, then the
    /// others, each group by `system_name` (the order [`refresh`] keeps).
    /// Hiding is a menu matter only, so a hidden app still autostarts.
    pub(super) fn autostart_ids(&self) -> Vec<&'static str> {
        self.apps
            .iter()
            .filter(|app| app.autostart)
            .map(|app| app.id)
            .collect()
    }

    /// Re-read the installed apps from `confd`. An unreachable `confd` keeps the
    /// previous list: a hiccup must not make installed apps vanish from the
    /// menu.
    pub(super) fn refresh(&mut self) {
        let Some(client) = self.client() else {
            return;
        };
        let Ok(keys) = client.list(PREFIX) else {
            self.endpoint = None;
            return;
        };
        let mut rows: Vec<pkgd::Installed> = Vec::new();
        for key in keys.iter().take(MAX_APPS) {
            if let Ok(Some(Value::Bytes(bytes))) = client.get(key) {
                if let Ok(row) = pkgd::decode_installed(&bytes) {
                    // The row must live at the key its own name derives.
                    if key.rsplit('/').next() == Some(row.system_name.as_str()) {
                        rows.push(row);
                    }
                }
            }
        }
        rows.sort_by(|a, b| {
            let core = |row: &pkgd::Installed| row.origin != pkgd::ORIGIN_CORE;
            core(a)
                .cmp(&core(b))
                .then_with(|| a.system_name.cmp(&b.system_name))
        });
        let apps = rows.into_iter().map(|row| self.app_of(row)).collect();
        self.apps = apps;
    }

    /// The caller's hidden apps: `user/<uid>/menu/hidden/*` over
    /// `sys/menu/hidden/*` (`deskmenu::hidden`). Nothing is hidden when
    /// `confd` does not answer.
    fn hidden(&mut self, uid: u32) -> Hidden {
        let Some(client) = self.client() else {
            return Hidden::default();
        };
        let mut pairs: Vec<(String, Value)> = Vec::new();
        for prefix in [hidden::user_prefix(uid), String::from(hidden::SYS_PREFIX)] {
            let Ok(keys) = client.list(&prefix) else {
                continue;
            };
            for key in keys.into_iter().take(MAX_HIDDEN_KEYS) {
                if let Ok(Some(value)) = client.get(&key) {
                    pairs.push((key, value));
                }
            }
        }
        Hidden::from_pairs(uid, pairs.iter().map(|(key, value)| (key.as_str(), value)))
    }

    /// The cached `confd` client, resolving on first use.
    fn client(&mut self) -> Option<Client> {
        if self.endpoint.is_none() {
            self.endpoint = registry::resolve(user::messenger::confd::NAME).ok();
        }
        self.endpoint
            .map(|endpoint| Client::from_endpoint(endpoint).with_timeout(CALL_TICKS))
    }

    fn app_of(&mut self, row: pkgd::Installed) -> InstalledApp {
        let id = self.intern(&row.system_name);
        let path = self.intern(&fhs::install_path(&row.install_dir, &row.binary));
        let label = self.intern(&format!("app:{}", row.system_name));
        InstalledApp {
            id,
            name: row.name,
            path,
            label,
            args: row.args,
            linux: row.abi == "linux",
            restart: Restart::OnFailure,
            core: row.origin == pkgd::ORIGIN_CORE,
            category: row.category,
            autostart: row.autostart,
            verbs: row.verbs,
            resident: row.resident,
            icon: fhs::icon_path(&row.install_dir),
        }
    }

    fn intern(&mut self, text: &str) -> &'static str {
        if let Some(known) = self.interned.iter().find(|known| **known == text) {
            return known;
        }
        let leaked: &'static str = alloc::boxed::Box::leak(String::from(text).into_boxed_str());
        self.interned.push(leaked);
        leaked
    }
}

/// The short id a core `system_name` answers to (`os.lazy.editor` ->
/// `editor`); `None` for any other app, so a user package can never claim a
/// bare id.
pub(super) fn alias_of(system_name: &str) -> Option<&str> {
    system_name
        .strip_prefix(CORE_PREFIX)
        .filter(|short| !short.is_empty() && !short.contains('.'))
}

/// Print the label an installed app was launched under, the evidence that the
/// kernel stamped it (`PKGD:LAUNCH:LABEL app:<system_name> pid=<pid>`): read
/// back from the kernel through `cred_get` and `label_name`, not echoed from the
/// request.
pub(super) fn report_label(pid: u64, id: &str) {
    let mut cred = sys::Cred::default();
    let mut name = [0u8; sys::MAX_LABEL_BYTES];
    let label = sys::cred_get(Some(pid), &mut cred)
        .ok()
        .and_then(|()| sys::label_name(cred.label_id, &mut name).ok())
        .and_then(|len| core::str::from_utf8(&name[..len]).ok());
    match label {
        Some(label) => sys::write_str(&format!("PKGD:LAUNCH:LABEL {label} pid={pid}\n")),
        None => sys::write_str(&format!("PKGD:LAUNCH:LABEL:FAIL {id} pid={pid}\n")),
    }
}

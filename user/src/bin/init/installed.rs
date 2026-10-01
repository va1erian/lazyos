//! The apps the package manager installed (`docs/packages.md`, phase 4): what
//! `ListApps` adds after the built-ins and what `Launch` resolves an unknown id
//! against.
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
use user::messenger::confd::Client;
use user::messenger::pkgd::wire as pkgd;
use user::messenger::{registry, services, Endpoint};
use user::sys;

/// The `confd` subtree `pkgd` writes (`pkgstore::layout::CONFD_PREFIX`).
const PREFIX: &str = "sys/apps";
/// Ticks one `confd` call may take (100 Hz): half a second.
const CALL_TICKS: u64 = 50;
/// Most installed apps `init` tracks, so a corrupt subtree cannot grow it
/// without bound.
const MAX_APPS: usize = 64;

/// One installed app, ready to launch.
pub(super) struct InstalledApp {
    /// The `system_name`: the app id `Launch` and the menu use.
    pub(super) id: &'static str,
    /// Display name from the manifest.
    pub(super) name: String,
    /// `/data/apps/<install_dir>/<binary>`.
    pub(super) path: &'static str,
    /// The kernel policy label, `app:<system_name>`.
    pub(super) label: &'static str,
    /// The manifest's fixed arguments, space-joined.
    pub(super) args: String,
    /// Whether it runs under the Linux ABI personality.
    pub(super) linux: bool,
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

    /// The installed app with this id.
    pub(super) fn find(&self, id: &str) -> Option<&InstalledApp> {
        self.apps.iter().find(|app| app.id == id.trim())
    }

    /// The installed apps as `ListApps` rows.
    pub(super) fn infos(&self) -> Vec<services::AppInfo> {
        self.apps
            .iter()
            .map(|app| services::AppInfo {
                id: app.id.to_string(),
                name: app.name.clone(),
                path: app.path.to_string(),
                restart: String::from("on-failure"),
                verbs: Vec::new(),
                installed: true,
            })
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
            a.installed_at
                .cmp(&b.installed_at)
                .then_with(|| a.system_name.cmp(&b.system_name))
        });
        let apps = rows.into_iter().map(|row| self.app_of(row)).collect();
        self.apps = apps;
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
            args: row.args.join(" "),
            linux: row.abi == "linux",
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

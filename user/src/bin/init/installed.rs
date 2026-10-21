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
//! A developer tool (`docs/devtool-packages-plan.md`) is started under the
//! label `tool:<system_name>` instead of `app:<system_name>`
//! (`pkgstore::rules::label_for_kind`, from the row's `kind`), and a tool row
//! that no administrator or image installed is not listed at all
//! (`pkgstore::toolgate::row_approved`): `pkgd` refuses to load it, and `init`
//! must not open an app whose label has no rules.
//!
//! `init` makes the same decision `pkgd`'s boot reconciliation makes before an
//! app is started under a tool label: for a tool row it reads the stored
//! manifest and applies `pkgstore::toolgate::stored_loadable` (the row and the
//! manifest agree on the class, the tool was approved, a manifest that claims
//! `shell` belongs to a package the image ships), cached by the row's digest.
//! A tool `pkgd` refused to load (its label was revoked) is therefore not
//! listed or launched either, instead of running as an unconfined process
//! whose Messenger rules are empty. (An application row needs no such check: its
//! `app:` label holds no rules unless `pkgd` loaded them.)
//!
//! A *shell* tool (`shell = true`, the Terminal; `pkgstore::shellrole::row_is_shell`,
//! decided from the row `pkgd` recorded and never from the package) is the
//! user's own shell: [`InstalledApps::owner_cred`] judges a caller running under
//! its label as the session's owner, so `powerctl` and a launch from the
//! Terminal work as before. It is also listed first, so the session opens it
//! before the other autostart apps as the built-in row it replaced did.
//!
//! Every call carries a deadline, so a wedged `confd` costs a request half a
//! second, never the supervisor.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use user::trace;

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
    /// The kernel policy label, `app:<system_name>` (`tool:<system_name>` for
    /// a developer tool).
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
    /// The uid that installed it (`Installed::installer`).
    pub(super) installer: u32,
    /// Installed by a system caller, for everyone (`Installed::machine_wide`).
    pub(super) machine_wide: bool,
    /// The user's shell (`shellrole::row_is_shell`): a core developer tool whose
    /// row says `shell`.
    pub(super) shell: bool,
}

impl InstalledApp {
    /// Whether it opens when a session of `uid` starts: it asks for it
    /// (`autostart`), and it is the image's own (core), or an administrator
    /// installed it for the machine, or `uid` installed it. An app one user
    /// installed never starts inside another user's session (audit H10).
    fn autostarts_for(&self, uid: u32) -> bool {
        pkgstore::access::autostarts_for(
            self.autostart,
            self.core,
            self.machine_wide,
            self.installer,
            uid,
        )
    }
}

/// The installed apps as of the last refresh.
pub(super) struct InstalledApps {
    apps: Vec<InstalledApp>,
    endpoint: Option<Endpoint>,
    /// Leaked strings by content: a service row holds `&'static str`s, and the
    /// user heap never frees them anyway, so each distinct string is leaked
    /// once and reused across refreshes.
    interned: Vec<&'static str>,
    /// The verdict of [`InstalledApps::stored_agrees`] for a tool row, by
    /// `(system_name, digest)`: a package's files change with its digest, so
    /// the manifest is read once per version. Only agreements are kept; a
    /// refusal or an unreadable manifest is looked at again on the next
    /// refresh.
    agreed: Vec<(String, String)>,
}

/// Largest stored manifest `init` reads back (the format's own cap is 1 MiB).
const MAX_MANIFEST: usize = 1024 * 1024;

impl InstalledApps {
    pub(super) const fn new() -> InstalledApps {
        InstalledApps {
            apps: Vec::new(),
            endpoint: None,
            interned: Vec::new(),
            agreed: Vec::new(),
        }
    }

    /// Whether a tool row's stored manifest still says what the row does and
    /// the tool may be started (`pkgstore::toolgate::stored_loadable`, the
    /// decision `pkgd` loads the label's rules by). Not a tool row: always.
    /// An unreadable manifest fails closed, as `pkgd`'s reconciliation does.
    fn stored_agrees(&mut self, row: &pkgd::Installed) -> bool {
        if !pkgstore::toolgate::is_tool_kind(&row.kind) {
            return true;
        }
        let known =
            |(name, digest): &(String, String)| *name == row.system_name && *digest == row.digest;
        if self.agreed.iter().any(known) {
            return true;
        }
        let Some(manifest) = stored_manifest(&row.install_dir) else {
            return false;
        };
        let core = row.origin == pkgd::ORIGIN_CORE;
        match pkgstore::toolgate::stored_loadable(&row.kind, &manifest, row.machine_wide, core) {
            Ok(()) => {
                self.agreed.retain(|(name, _)| *name != row.system_name);
                self.agreed
                    .push((row.system_name.clone(), row.digest.clone()));
                true
            }
            Err(why) => {
                trace::line!(
                    trace::Channel::Init,
                    "APPS:REFUSE {}: {why}",
                    row.system_name
                );
                false
            }
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

    /// The ids that open when a session of `uid` starts: core apps first, then
    /// the others, each group by `system_name` (the order [`refresh`] keeps).
    /// Hiding is a menu matter only, so a hidden app still autostarts.
    pub(super) fn autostart_ids(&self, uid: u32) -> Vec<&'static str> {
        self.apps
            .iter()
            .filter(|app| app.autostarts_for(uid))
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
            let not_shell = |row: &pkgd::Installed| !is_shell(row);
            core(a)
                .cmp(&core(b))
                .then_with(|| not_shell(a).cmp(&not_shell(b)))
                .then_with(|| a.system_name.cmp(&b.system_name))
        });
        let mut apps = Vec::new();
        for row in rows.into_iter().filter(approved) {
            if self.stored_agrees(&row) {
                apps.push(self.app_of(row));
            }
        }
        self.apps = apps;
    }

    /// `cred` as `init` judges its sender: a task running under a shell tool's
    /// label (the Terminal's own and everything it starts) is the session's
    /// owner, so its label is dropped; any other cred is returned as it is.
    /// The label is named by the kernel (`init` holds the capability to read
    /// it) and the shell flag comes from the row `pkgd` recorded. The apps are
    /// re-read once when the package is not known yet.
    pub(super) fn owner_cred(&mut self, cred: sys::Cred) -> sys::Cred {
        if cred.label_id == 0 {
            return cred;
        }
        let mut name = [0u8; sys::MAX_LABEL_BYTES];
        let Some(system_name) = sys::label_name(cred.label_id, &mut name)
            .ok()
            .and_then(|len| core::str::from_utf8(&name[..len]).ok())
            .and_then(pkgstore::toolgate::tool_system_name)
        else {
            return cred;
        };
        if !self.apps.iter().any(|app| app.id == system_name) {
            self.refresh();
        }
        let shell = self
            .apps
            .iter()
            .any(|app| app.id == system_name && app.shell);
        owner_of(cred, shell)
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
        let label = self.intern(&label_of(&row));
        let shell = is_shell(&row);
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
            installer: row.installer,
            machine_wide: row.machine_wide,
            shell,
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

/// The manifest `pkgd` stored when it installed `install_dir`; `None` when it
/// cannot be read or parsed.
fn stored_manifest(install_dir: &str) -> Option<lazypkg::Manifest> {
    let root = pkgstore::layout::install_path(install_dir).ok()?;
    let path = alloc::format!("{root}/{}", pkgstore::layout::MANIFEST_FILE);
    let bytes = user::files::read_up_to(&path, MAX_MANIFEST).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    lazypkg::parse_manifest(&text).ok()
}

/// The policy label an installed app is started under.
fn label_of(row: &pkgd::Installed) -> String {
    pkgstore::rules::label_for_kind(&row.system_name, &row.kind)
}

/// Whether a row records the user's shell: the row's own `kind`, `shell` and
/// origin, as `pkgd` wrote them.
fn is_shell(row: &pkgd::Installed) -> bool {
    let core = row.origin == pkgd::ORIGIN_CORE;
    pkgstore::shellrole::row_is_shell(&row.kind, row.shell, core)
}

/// `cred` with its label dropped when `shell` (see [`InstalledApps::owner_cred`]).
fn owner_of(cred: sys::Cred, shell: bool) -> sys::Cred {
    if shell {
        sys::Cred::new(cred.uid, cred.gid, cred.caps, 0, cred.session)
    } else {
        cred
    }
}

/// Whether a row may be used: not a developer tool, or one an administrator
/// approved or the image ships.
fn approved(row: &pkgd::Installed) -> bool {
    let core = row.origin == pkgd::ORIGIN_CORE;
    pkgstore::toolgate::row_approved(&row.kind, row.machine_wide, core)
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
    let mut name = [0u8; sys::MAX_LABEL_BYTES];
    let label = sys::cred_get(Some(pid))
        .ok()
        .and_then(|cred| sys::label_name(cred.label_id, &mut name).ok())
        .and_then(|len| core::str::from_utf8(&name[..len]).ok());
    match label {
        Some(label) => trace::line!(trace::Channel::Pkgd, "LAUNCH:LABEL {label} pid={pid}"),
        None => trace::line!(trace::Channel::Pkgd, "LAUNCH:LABEL:FAIL {id} pid={pid}"),
    }
}

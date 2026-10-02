//! The installer's pure state machine and its package vocabulary.
//!
//! The model owns the whole screen state: which screen is shown, the installed
//! list, the package under consent, any queued request and the status banner.
//! The app in `src/bin/installer.rs` drives it; because it is pure, the
//! transitions (including the failure paths) are unit-tested here rather than
//! through the widgets.

use super::view::clean;

/// One handled file type, as the consent screen shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MimeHandler {
    /// The `type/subtype` string.
    pub mime_type: String,
    /// The verbs the package registers (`open`, `edit`, ...).
    pub verbs: Vec<String>,
    /// Whether the package ships icons for this type.
    pub has_icon: bool,
}

/// One requested permission with the explanation `pkgd` supplied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Permission {
    /// `interface`, `topic`, `file` or `network`.
    pub kind: String,
    /// The concrete value (`os.lazy.clipboard.v1`, `read:$HOME/*`, ...).
    pub value: String,
    /// `high`, `medium`, `low`, or anything else (treated as "other").
    pub risk: String,
    /// The friendly sentence the consent screen shows.
    pub explanation: String,
}

/// A package `pkgd` inspected. Every field is untrusted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    /// Display name.
    pub name: String,
    /// Reverse-DNS id.
    pub system_name: String,
    /// The (unverified) author string.
    pub author: String,
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Optional prose.
    pub description: String,
    /// Lowercase hex SHA-256 of the archive.
    pub digest: String,
    /// Install directory relative to `/apps`.
    pub install_dir: String,
    /// Handled file types.
    pub mime: Vec<MimeHandler>,
    /// Requested permissions.
    pub permissions: Vec<Permission>,
    /// Non-empty when the package cannot be installed.
    pub problems: Vec<String>,
    /// The menu group (`lazypkg::Category`).
    pub category: String,
    /// Whether the app asks to start when the user logs in.
    pub autostart: bool,
}

/// One installed app, as `pkgd`'s `List`/`Install` report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed {
    /// Reverse-DNS id.
    pub system_name: String,
    /// Display name.
    pub name: String,
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Install directory relative to `/apps`.
    pub install_dir: String,
    /// Lowercase hex SHA-256 of the archive.
    pub digest: String,
    /// Entry binary relative to the install directory.
    pub binary: String,
    /// Kernel ticks at install time.
    pub installed_at: u64,
    /// Shipped with LazyOS (a core package, issue #509): it cannot be
    /// removed, only hidden from the menu in Settings.
    pub core: bool,
}

/// Why a core app has no Remove button (the same words `pkgd` refuses with).
pub fn core_removal_refused(name: &str) -> String {
    format!(
        "{} is part of LazyOS and can't be removed; you can hide it from the menu in Settings.",
        clean(name)
    )
}

/// Which screen the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// The installed list plus the "open a package" field.
    #[default]
    List,
    /// The consent screen for [`Model::inspected`].
    Consent,
    /// The progress screen while `Install` runs.
    Installing,
    /// The success screen after `Install` returned.
    Done,
    /// The confirmation before `Remove`.
    ConfirmRemove,
}

/// A request the app queued but has not completed. It is dropped whenever the
/// user navigates away, so a stale path or name can never be replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Inspect `path`.
    Inspect(String),
    /// Install `path`.
    Install(String),
    /// Remove `system_name`.
    Remove(String),
}

/// The installer's whole state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// The active screen.
    pub screen: Screen,
    /// The last `List` result, in install order.
    pub packages: Vec<Installed>,
    /// Whether `List` has answered (so an empty list is "none", not "loading").
    pub list_loaded: bool,
    /// What the user typed in the "open a package" field.
    pub path_input: String,
    /// The package the consent screen is showing.
    pub inspected: Option<Package>,
    /// The path [`Model::inspected`] was read from (what `Install` will use).
    pub inspected_path: Option<String>,
    /// The app the confirmation screen is about.
    pub pending_remove: Option<Installed>,
    /// A status line (an error, or a friendly note).
    pub banner: Option<String>,
    /// The in-flight request, if any.
    pub pending: Option<Request>,
    /// The app `Install` just confirmed, for the success screen.
    pub last_installed: Option<Installed>,
}

impl Model {
    /// A fresh model on the (empty) list screen.
    pub fn new() -> Model {
        Model::default()
    }

    /// Drops everything that belongs to a screen the user is leaving: the
    /// inspected package, the remove target, the queued request and the
    /// banner. The installed list and the typed path are *not* transient.
    fn clear_transient(&mut self) {
        self.inspected = None;
        self.inspected_path = None;
        self.pending_remove = None;
        self.pending = None;
        self.banner = None;
        self.last_installed = None;
    }

    /// The apps the user installed first, then the built-in ones, each group
    /// in `pkgd`'s order: the rows with a Remove button stay at the top
    /// however many core apps the image ships.
    fn order_rows(&mut self) {
        self.packages.sort_by_key(|app| app.core);
    }

    /// Records what the user typed in the path field.
    pub fn set_path(&mut self, text: &str) {
        self.path_input = text.to_owned();
    }

    /// `List` answered. Replaces the list and returns to it.
    pub fn list_loaded(&mut self, packages: Vec<Installed>) {
        self.packages = packages;
        self.order_rows();
        self.list_loaded = true;
        self.clear_transient();
        self.screen = Screen::List;
    }

    /// `List` failed. The previous list stays on screen with the error text.
    pub fn list_failed(&mut self, reason: impl Into<String>) {
        self.list_loaded = true;
        self.clear_transient();
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::List;
    }

    /// `Inspect` succeeded: show the consent screen for `package`, which was
    /// read from `path`. The typed path is cleared so `q` quits again once the
    /// user is back on the list.
    pub fn inspect_ok(&mut self, path: String, package: Package) {
        self.clear_transient();
        self.inspected = Some(package);
        self.inspected_path = Some(path);
        self.path_input.clear();
        self.screen = Screen::Consent;
    }

    /// `Inspect` failed: stay on the list and show why.
    pub fn inspect_failed(&mut self, reason: impl Into<String>) {
        self.clear_transient();
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::List;
    }

    /// The user accepted the consent: queue the install and show progress.
    /// The request is deferred so the progress screen can paint before the
    /// (blocking) service call.
    pub fn install_started(&mut self) {
        let path = self.inspected_path.clone().unwrap_or_default();
        self.banner = None;
        self.pending = Some(Request::Install(path));
        self.screen = Screen::Installing;
    }

    /// `Install` succeeded. The returned app is added to the list — only now,
    /// never before `pkgd` confirmed it — and the success screen is shown.
    pub fn install_ok(&mut self, app: Installed) {
        self.clear_transient();
        match self
            .packages
            .iter_mut()
            .find(|installed| installed.system_name == app.system_name)
        {
            Some(slot) => *slot = app.clone(),
            None => self.packages.push(app.clone()),
        }
        self.order_rows();
        self.last_installed = Some(app);
        self.screen = Screen::Done;
    }

    /// `Install` failed: the consent screen stays up with the error, and the
    /// package is *not* shown as installed.
    pub fn install_failed(&mut self, reason: impl Into<String>) {
        self.pending = None;
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::Consent;
    }

    /// The user asked to remove `app`: ask for confirmation first. A core
    /// app is refused here, before any confirmation, whatever sent the
    /// request (its row has no Remove button, but a message is a message);
    /// the list stays up with the reason. Returns whether the confirmation
    /// is now showing.
    pub fn remove_asked(&mut self, app: Installed) -> bool {
        let core = app.core
            || self
                .packages
                .iter()
                .any(|listed| listed.core && listed.system_name == app.system_name);
        if core {
            self.clear_transient();
            self.banner = Some(core_removal_refused(&app.name));
            self.screen = Screen::List;
            return false;
        }
        self.banner = None;
        self.pending_remove = Some(app);
        self.screen = Screen::ConfirmRemove;
        true
    }

    /// The listed core app the package under consent would update, if any.
    pub fn updates_core(&self) -> Option<&Installed> {
        let package = self.inspected.as_ref()?;
        self.packages
            .iter()
            .find(|app| app.core && app.system_name == package.system_name)
    }

    /// What the consent screen says about the package besides its
    /// permissions: that it updates a built-in app, and that it starts when
    /// the user logs in (honoured only because the user consents here).
    pub fn consent_notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(app) = self.updates_core() {
            notes.push(format!("Updates built-in app {}", clean(&app.name)));
        }
        if self.inspected.as_ref().is_some_and(|package| package.autostart) {
            notes.push("Starts when you log in".to_owned());
        }
        notes
    }

    /// `Remove` succeeded. The app leaves the list and the banner is cleared.
    pub fn remove_ok(&mut self, system_name: &str) {
        self.clear_transient();
        self.packages.retain(|app| app.system_name != system_name);
        self.screen = Screen::List;
    }

    /// `Remove` failed: the list is unchanged and the error is shown.
    pub fn remove_failed(&mut self, reason: impl Into<String>) {
        self.pending = None;
        self.pending_remove = None;
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::List;
    }

    /// Esc / Cancel / Close: return to the list, dropping the transient state.
    /// A running install cannot be cancelled.
    pub fn cancel(&mut self) {
        if self.screen == Screen::Installing {
            return;
        }
        self.clear_transient();
        self.screen = Screen::List;
    }

    /// The success screen's "Done": the same as [`Model::cancel`].
    pub fn done(&mut self) {
        self.cancel();
    }

    /// Takes the queued request, leaving none behind.
    pub fn take_pending(&mut self) -> Option<Request> {
        self.pending.take()
    }

    /// The system name the confirmation screen would remove.
    pub fn pending_remove_name(&self) -> Option<String> {
        self.pending_remove
            .as_ref()
            .map(|app| app.system_name.clone())
    }
}

#[cfg(test)]
mod tests;

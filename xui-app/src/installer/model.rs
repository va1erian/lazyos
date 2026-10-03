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
///
/// Installing a package is a wizard: [`Screen::Choose`], [`Screen::Review`],
/// [`Screen::Permissions`], then [`Screen::Installing`] and [`Screen::Done`]
/// (both the last step). [`Screen::step`] numbers them for the step header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// The installed list.
    #[default]
    List,
    /// Wizard step 1: pick the `.lzp` (a path field and a file picker).
    Choose,
    /// Wizard step 2: what the package is, or why it cannot be installed.
    Review,
    /// Wizard step 3: the consent to the package's permissions.
    Permissions,
    /// Wizard step 4: the progress screen while `Install` runs.
    Installing,
    /// Wizard step 4: the success screen after `Install` returned.
    Done,
    /// The confirmation before `Remove`.
    ConfirmRemove,
}

/// The wizard's step titles, in order; [`Screen::step`] indexes them.
pub const WIZARD_STEPS: [&str; 4] = ["Choose", "Review", "Permissions", "Install"];

impl Screen {
    /// The wizard step (an index into [`WIZARD_STEPS`]) this screen belongs
    /// to, or `None` outside the wizard.
    pub fn step(self) -> Option<usize> {
        match self {
            Screen::Choose => Some(0),
            Screen::Review => Some(1),
            Screen::Permissions => Some(2),
            Screen::Installing | Screen::Done => Some(3),
            Screen::List | Screen::ConfirmRemove => None,
        }
    }

    /// The name the serial evidence uses for the screen.
    pub fn marker(self) -> &'static str {
        match self {
            Screen::List => "LIST",
            Screen::Choose => "CHOOSE",
            Screen::Review => "REVIEW",
            Screen::Permissions => "PERMISSIONS",
            Screen::Installing => "INSTALLING",
            Screen::Done => "DONE",
            Screen::ConfirmRemove => "CONFIRM_REMOVE",
        }
    }
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
    /// The package path on the Choose step (typed or picked).
    pub path_input: String,
    /// The package the Review and Permissions steps are showing.
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

    /// "Install a package…": open the wizard on the Choose step, prefilled
    /// with the last path the user chose.
    pub fn start_wizard(&mut self) {
        self.clear_transient();
        self.screen = Screen::Choose;
    }

    /// The file picker returned `path`: it becomes the Choose step's path.
    pub fn path_picked(&mut self, path: &str) {
        self.banner = None;
        self.path_input = path.to_owned();
    }

    /// `Inspect` succeeded: show the Review step for `package`, which was read
    /// from `path`. The path stays in the field so Back returns to it.
    pub fn inspect_ok(&mut self, path: String, package: Package) {
        self.clear_transient();
        self.path_input = path.clone();
        self.inspected = Some(package);
        self.inspected_path = Some(path);
        self.screen = Screen::Review;
    }

    /// `Inspect` failed: stay on (or go to) the Choose step and show why.
    pub fn inspect_failed(&mut self, reason: impl Into<String>) {
        self.clear_transient();
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::Choose;
    }

    /// Whether the Review step may advance: a package without problems.
    pub fn can_advance(&self) -> bool {
        self.screen == Screen::Review && self.installable()
    }

    /// Whether the Permissions step may install: a package without problems
    /// and no install already queued.
    pub fn can_install(&self) -> bool {
        self.screen == Screen::Permissions && self.pending.is_none() && self.installable()
    }

    /// Whether the inspected package has no problems.
    fn installable(&self) -> bool {
        self.inspected
            .as_ref()
            .is_some_and(|package| package.problems.is_empty())
    }

    /// Next on the Review step: on to the permissions. Returns whether the
    /// screen changed (a package with problems never gets past Review).
    pub fn advance(&mut self) -> bool {
        if !self.can_advance() {
            return false;
        }
        self.banner = None;
        self.screen = Screen::Permissions;
        true
    }

    /// Back: one wizard step earlier. Leaving Review drops the package (the
    /// path stays in the field), and Back from Choose leaves the wizard.
    /// Screens outside the wizard's editable steps ignore it.
    pub fn back(&mut self) {
        match self.screen {
            Screen::Permissions => {
                self.banner = None;
                self.pending = None;
                self.screen = Screen::Review;
            }
            Screen::Review => {
                let path = self.inspected_path.take();
                self.clear_transient();
                if let Some(path) = path {
                    self.path_input = path;
                }
                self.screen = Screen::Choose;
            }
            Screen::Choose => self.cancel(),
            Screen::List | Screen::Installing | Screen::Done | Screen::ConfirmRemove => {}
        }
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

    /// `Install` failed: the Permissions step comes back with the error (so
    /// the user can retry), and the package is *not* shown as installed.
    pub fn install_failed(&mut self, reason: impl Into<String>) {
        self.pending = None;
        self.banner = Some(clean(&reason.into()));
        self.screen = Screen::Permissions;
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
        if self
            .inspected
            .as_ref()
            .is_some_and(|package| package.autostart)
        {
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

    /// Esc / Cancel / Close: leave the wizard (or the confirmation) for the
    /// list, dropping the transient state. A running install cannot be
    /// cancelled.
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

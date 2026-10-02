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
    /// The concrete value (`os.lazy.clipboard.v1`, `read:/home/*`, ...).
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

    /// Records what the user typed in the path field.
    pub fn set_path(&mut self, text: &str) {
        self.path_input = text.to_owned();
    }

    /// `List` answered. Replaces the list and returns to it.
    pub fn list_loaded(&mut self, packages: Vec<Installed>) {
        self.packages = packages;
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

    /// The user asked to remove `app`: ask for confirmation first.
    pub fn remove_asked(&mut self, app: Installed) {
        self.banner = None;
        self.pending_remove = Some(app);
        self.screen = Screen::ConfirmRemove;
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
mod tests {
    use super::*;

    fn installed(system_name: &str, version: &str) -> Installed {
        Installed {
            system_name: system_name.to_owned(),
            name: system_name
                .rsplit('.')
                .next()
                .unwrap_or(system_name)
                .to_owned(),
            version: version.to_owned(),
            ..Installed::default()
        }
    }

    fn package(system_name: &str, problems: &[&str]) -> Package {
        Package {
            name: "Paint".into(),
            system_name: system_name.into(),
            version: "1.0.0".into(),
            problems: problems.iter().map(|p| (*p).to_owned()).collect(),
            ..Package::default()
        }
    }

    #[test]
    fn a_fresh_model_is_an_empty_list() {
        let model = Model::new();
        assert_eq!(model.screen, Screen::List);
        assert!(model.packages.is_empty());
        assert!(!model.list_loaded);
        assert!(model.banner.is_none());
    }

    #[test]
    fn the_consent_flow_reaches_done_and_returns_to_the_list() {
        let mut model = Model::new();
        model.set_path("/transient/paint.lzp");
        model.inspect_ok("/transient/paint.lzp".into(), package("org.lazy.paint", &[]));
        assert_eq!(model.screen, Screen::Consent);
        assert_eq!(model.inspected_path.as_deref(), Some("/transient/paint.lzp"));
        // Inspecting clears the typed path so `q` quits again from the list.
        assert!(model.path_input.is_empty());

        model.install_started();
        assert_eq!(model.screen, Screen::Installing);
        assert_eq!(
            model.pending,
            Some(Request::Install("/transient/paint.lzp".into()))
        );

        let app = installed("org.lazy.paint", "1.0.0");
        model.install_ok(app.clone());
        assert_eq!(model.screen, Screen::Done);
        assert_eq!(model.packages, vec![app.clone()]);
        assert_eq!(model.last_installed.as_ref(), Some(&app));
        assert!(
            model.inspected.is_none(),
            "the package is no longer pending"
        );
        assert!(model.pending.is_none());

        model.done();
        assert_eq!(model.screen, Screen::List);
        assert!(
            model.last_installed.is_none(),
            "Done drops the success data"
        );
        assert_eq!(model.packages, vec![app], "the confirmed app stays listed");
    }

    #[test]
    fn a_package_with_problems_offers_only_close() {
        let mut model = Model::new();
        model.inspect_ok(
            "/transient/bad.lzp".into(),
            package("org.lazy.bad", &["version \"1\" is not semver"]),
        );
        assert_eq!(model.screen, Screen::Consent);
        assert_eq!(model.inspected.as_ref().unwrap().problems.len(), 1);
        model.cancel();
        assert_eq!(model.screen, Screen::List);
        assert!(model.inspected.is_none(), "Close drops the package");
    }

    #[test]
    fn inspect_failure_stays_on_the_list_with_the_reason() {
        let mut model = Model::new();
        model.inspect_failed("not a zip archive");
        assert_eq!(model.screen, Screen::List);
        assert_eq!(model.banner.as_deref(), Some("not a zip archive"));
        assert!(model.inspected.is_none());
    }

    #[test]
    fn inspect_again_drops_the_previous_package_and_pending_request() {
        let mut model = Model::new();
        model.inspect_ok("/transient/a.lzp".into(), package("org.lazy.a", &[]));
        model.install_started();
        assert!(model.pending.is_some());
        // A second inspect while the first is pending must not reuse the first.
        model.inspect_ok("/transient/b.lzp".into(), package("org.lazy.b", &[]));
        assert_eq!(model.inspected.as_ref().unwrap().system_name, "org.lazy.b");
        assert!(model.pending.is_none(), "the stale install was dropped");
        assert_eq!(model.inspected_path.as_deref(), Some("/transient/b.lzp"));
    }

    #[test]
    fn install_failure_keeps_the_package_and_shows_the_error() {
        let mut model = Model::new();
        model.inspect_ok("/transient/paint.lzp".into(), package("org.lazy.paint", &[]));
        model.install_started();
        model.install_failed("pkgd error 13");
        assert_eq!(model.screen, Screen::Consent);
        assert_eq!(model.banner.as_deref(), Some("pkgd error 13"));
        assert!(model.inspected.is_some(), "the user can retry");
        assert!(model.pending.is_none());
        assert!(
            model.packages.is_empty(),
            "nothing is listed before pkgd confirms it"
        );
    }

    #[test]
    fn the_remove_flow_confirms_then_updates_the_list() {
        let mut model = Model::new();
        model.list_loaded(vec![
            installed("org.lazy.a", "1.0.0"),
            installed("org.lazy.b", "2.0.0"),
        ]);
        model.remove_asked(model.packages[0].clone());
        assert_eq!(model.screen, Screen::ConfirmRemove);
        assert_eq!(model.pending_remove_name().as_deref(), Some("org.lazy.a"));
        model.remove_ok("org.lazy.a");
        assert_eq!(model.screen, Screen::List);
        assert_eq!(model.packages.len(), 1);
        assert_eq!(model.packages[0].system_name, "org.lazy.b");
        assert!(model.pending_remove.is_none());
        assert!(model.banner.is_none());
    }

    #[test]
    fn a_failed_remove_keeps_the_app_and_shows_the_error() {
        let mut model = Model::new();
        model.list_loaded(vec![installed("org.lazy.a", "1.0.0")]);
        model.remove_asked(model.packages[0].clone());
        model.remove_failed("permission denied");
        assert_eq!(model.screen, Screen::List);
        assert_eq!(model.packages.len(), 1, "the app is still installed");
        assert_eq!(model.banner.as_deref(), Some("permission denied"));
        assert!(model.pending_remove.is_none());
    }

    #[test]
    fn cancel_does_not_interrupt_a_running_install() {
        let mut model = Model::new();
        model.inspect_ok("/transient/paint.lzp".into(), package("org.lazy.paint", &[]));
        model.install_started();
        model.cancel();
        assert_eq!(model.screen, Screen::Installing);
        assert!(model.pending.is_some());
    }

    #[test]
    fn list_failure_keeps_the_previous_rows() {
        let mut model = Model::new();
        model.list_loaded(vec![installed("org.lazy.a", "1.0.0")]);
        model.list_failed("pkgd is unavailable");
        assert_eq!(model.screen, Screen::List);
        assert_eq!(model.packages.len(), 1, "the last good list stays");
        assert_eq!(model.banner.as_deref(), Some("pkgd is unavailable"));
    }

    #[test]
    fn control_characters_never_reach_the_banner() {
        let mut model = Model::new();
        model.inspect_failed("bad\nreason\u{7}");
        assert_eq!(model.banner.as_deref(), Some("badreason"));
    }

    #[test]
    fn a_second_install_of_the_same_id_replaces_the_row() {
        let mut model = Model::new();
        model.list_loaded(vec![installed("org.lazy.paint", "1.0.0")]);
        model.install_ok(installed("org.lazy.paint", "2.0.0"));
        assert_eq!(model.packages.len(), 1);
        assert_eq!(model.packages[0].version, "2.0.0");
    }
}

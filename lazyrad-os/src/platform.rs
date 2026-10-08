//! The [`Platform`] LazyOS installs into the LazyRAD runtime.
//!
//! It answers four questions the portable crates cannot:
//!
//! * which files a script may touch (decision D5: an app's private data
//!   directory, its own project read-only, the documents it was started to
//!   open read-only; [`crate::policy`]);
//! * which files those are (`app.documents`);
//! * where LazyRAD keeps settings and projects (the user's home, [`Home`]);
//! * where the player binary is (`lrplay.elf` beside the running IDE in its
//!   install directory, [`player_beside`]);
//! * the monospace face (the bundled JetBrains Mono).
//!
//! Everything LazyRAD writes lives in the home of the user running it
//! (filesystem plan F4): `init` passes `HOME` to every session app. Installed
//! apps never write inside `/apps`, which belongs to `pkgd`.
//!
//! Dialogs are left at the trait's "cancel" default here: LazyOS dialogs are
//! painted in-window (xui's portable `FileDialog`), so the IDE drives them
//! through its own widgets instead of a blocking call (see `bin/lazyrad.rs`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lazyrad_runtime::platform::{Platform, ScriptPermissions};
use lazyrad_runtime::FsPolicy;
use xui_core::widget::StdFileSystem;

pub use crate::policy::{data_root, installed_app_id, player_policy, PolicySpec};

/// Where installed apps live (`docs/packages.md`: `install_dir` is relative to
/// this). Read only: `pkgd` writes it.
pub const APPS_ROOT: &str = fhs::state::APPS_ROOT;

/// The player's file name inside the package, beside `lazyrad.elf`
/// (`bin/lrplay.elf`, `tools/xui/core_packages.py`).
pub const PLAYER_FILE: &str = "lrplay.elf";

/// The player that ships with the IDE at `exe`: the same install directory
/// (`/apps/os.lazy.lazyrad/<version>-<hash>/bin/`). The packager copies it into
/// every `.lzp` it builds, and Play runs it.
pub fn player_beside(exe: &Path) -> PathBuf {
    exe.with_file_name(PLAYER_FILE)
}

/// The keyboard service every packaged player needs (see
/// [`LazyOsPlatform::script_permissions`]).
pub const PLAYER_INPUT: &str = "os.lazy.input.v1";

/// The home used when `$HOME` is unset or unusable: the ramfs, so nothing is
/// kept across a reboot.
pub const SCRATCH_DIR: &str = fhs::state::LAZYRAD_TMP;

/// The longest `$HOME` accepted, in bytes.
const MAX_HOME_BYTES: usize = 1024;

/// The home LazyRAD keeps its files in, and every place derived from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    path: PathBuf,
    fallback: bool,
}

impl Home {
    /// The home named by `value` (the `HOME` variable): an absolute path with
    /// no `.`/`..` component and no control character. Anything else, or no
    /// value, gives [`SCRATCH_DIR`] and [`is_fallback`](Self::is_fallback).
    pub fn from_var(value: Option<&OsStr>) -> Home {
        match value.and_then(OsStr::to_str).filter(|v| usable_home(v)) {
            Some(path) => Home {
                path: PathBuf::from(path),
                fallback: false,
            },
            None => Home {
                path: PathBuf::from(SCRATCH_DIR),
                fallback: true,
            },
        }
    }

    /// The home of this process, from `$HOME`.
    pub fn from_env() -> Home {
        Home::from_var(std::env::var_os("HOME").as_deref())
    }

    /// The home directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `$HOME` was missing, so files go to the ramfs.
    pub fn is_fallback(&self) -> bool {
        self.fallback
    }

    /// The warning to show when [`is_fallback`](Self::is_fallback).
    pub fn fallback_warning(&self) -> Option<String> {
        self.fallback.then(|| {
            format!(
                "HOME is not set; using {} (nothing is kept after a reboot)",
                self.path.display()
            )
        })
    }

    /// The data directory of the app `system_name`, `<home>/.apps/<name>`.
    pub fn app_data(&self, system_name: &str) -> PathBuf {
        PathBuf::from(fhs::app_data_dir(&self.path_text(), system_name))
    }

    /// The IDE's settings, `<home>/.apps/os.lazy.lazyrad/config`.
    pub fn config_dir(&self) -> PathBuf {
        self.app_data(fhs::state::LAZYRAD_APP)
            .join(fhs::state::LAZYRAD_CONFIG)
    }

    /// Where the IDE keeps projects, `<home>/projects`.
    pub fn projects_dir(&self) -> PathBuf {
        self.path.join(fhs::state::LAZYRAD_PROJECTS)
    }

    /// The read/write directory of a project that is not an installed app,
    /// `<home>/.apps/os.lazy.lazyrad/data`.
    pub fn dev_data_dir(&self) -> PathBuf {
        self.app_data(fhs::state::LAZYRAD_APP)
            .join(fhs::state::LAZYRAD_DATA)
    }

    fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// An absolute path below `/` of reasonable length, with no empty, `.` or
/// `..` segment and no control character: the only `$HOME` the sandbox is
/// rooted at.
fn usable_home(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('/') else {
        return false;
    };
    value.len() <= MAX_HOME_BYTES
        && !value.chars().any(char::is_control)
        && rest
            .trim_end_matches('/')
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// The folder the file dialog starts in: the projects folder, else the home,
/// else `/transient` (the first `exists` answers yes for), else `/`.
pub fn start_dir(home: &Home, exists: impl Fn(&Path) -> bool) -> PathBuf {
    [
        home.projects_dir(),
        home.path().to_path_buf(),
        PathBuf::from(fhs::mount::TRANSIENT),
    ]
    .into_iter()
    .find(|path| exists(path))
    .unwrap_or_else(|| PathBuf::from("/"))
}

/// What the player and the IDE share on LazyOS.
pub struct LazyOsPlatform {
    policy: PolicySpec,
    home: Home,
    player: Option<PathBuf>,
    documents: Vec<PathBuf>,
}

impl LazyOsPlatform {
    /// The platform for a player whose scripts run under `policy` and were
    /// started to open `documents` (which `policy` must let them read). A
    /// player starts no other player.
    pub fn player(policy: PolicySpec, home: Home, documents: Vec<PathBuf>) -> LazyOsPlatform {
        LazyOsPlatform {
            policy,
            home,
            player: None,
            documents,
        }
    }

    /// The platform for the IDE running from `exe`: scripts (designer preview)
    /// may use the projects folder only, and the player is the one beside
    /// `exe` ([`player_beside`]).
    pub fn ide(home: Home, exe: &Path) -> LazyOsPlatform {
        LazyOsPlatform {
            policy: PolicySpec::new(home.projects_dir()),
            home,
            player: Some(player_beside(exe)),
            documents: Vec::new(),
        }
    }
}

impl Platform for LazyOsPlatform {
    fn name(&self) -> &'static str {
        "lazyos"
    }

    fn config_dir(&self) -> Option<PathBuf> {
        Some(self.home.config_dir())
    }

    fn default_monospace_font(&self) -> &'static str {
        xui_app::font::MONO_FAMILY
    }

    fn fs_policy(&self) -> FsPolicy {
        self.policy.build()
    }

    fn documents(&self) -> Vec<PathBuf> {
        self.documents.clone()
    }

    fn prefers_dark(&self) -> bool {
        crate::desktop_mode::is_dark()
    }

    fn system_theme(&self) -> Option<xui_core::Theme> {
        crate::desktop_mode::theme()
    }

    fn player_executable(&self) -> Option<PathBuf> {
        self.player.clone()
    }

    /// The portable dialog over the shim's `std::fs`; the VFS lists mount
    /// points in `/` itself.
    fn file_system(&self) -> Option<Rc<dyn xui_core::widget::FileSystem>> {
        Some(Rc::new(StdFileSystem))
    }

    fn projects_dir(&self) -> PathBuf {
        start_dir(&self.home, |path| path.is_dir())
    }

    /// The interfaces and topics the scripts' `sys::*` and literal `msg::*`
    /// calls need (`rhai_lazy::msg::permissions`), plus the mixer's for a
    /// script that plays a song (`crate::tracker`), so an installed app is
    /// granted exactly those.
    fn script_permissions(&self, scripts: &[&str]) -> ScriptPermissions {
        let found = rhai_lazy::msg::permissions::derive(scripts.iter().copied());
        let mut interfaces = found.interfaces;
        for interface in crate::tracker::script_interfaces(scripts.iter().copied()) {
            if !interfaces.contains(&interface) {
                interfaces.push(interface);
            }
        }
        // The player itself, whatever the scripts do: the packager adds the
        // display for its window, but its keyboard comes from `inputd`, which
        // a labelled player may not resolve without this (a `LABEL:DENY
        // resolve=os.lazy.input.v1` in a development run, issue #529).
        if !interfaces.iter().any(|name| name == PLAYER_INPUT) {
            interfaces.push(PLAYER_INPUT.to_owned());
        }
        interfaces.sort();
        ScriptPermissions {
            interfaces,
            topics: found.topics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDE_EXE: &str = "/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lazyrad.elf";

    fn home(path: &str) -> Home {
        Home::from_var(Some(OsStr::new(path)))
    }

    #[test]
    fn the_home_comes_from_home_and_falls_back_to_the_ramfs() {
        let user = home("/home/user");
        assert_eq!(user.path(), Path::new("/home/user"));
        assert!(!user.is_fallback());
        assert_eq!(user.fallback_warning(), None);

        let unset = Home::from_var(None);
        assert_eq!(unset.path(), Path::new(SCRATCH_DIR));
        assert!(unset.is_fallback());
        let warning = unset.fallback_warning().expect("a visible warning");
        assert!(warning.contains(SCRATCH_DIR), "{warning}");
    }

    #[test]
    fn an_unusable_home_is_the_fallback() {
        let long = format!("/{}", "x".repeat(MAX_HOME_BYTES));
        for bad in [
            "",
            "home/user",
            "/home/../etc",
            "/home/./user",
            "/home//user",
            "/",
            "/home/a\nb",
            &long,
        ] {
            assert!(home(bad).is_fallback(), "{bad:?}");
        }
    }

    #[test]
    fn lazyrad_places_live_in_the_home() {
        let user = home("/home/user");
        assert_eq!(
            user.config_dir(),
            Path::new("/home/user/.apps/os.lazy.lazyrad/config")
        );
        assert_eq!(user.projects_dir(), Path::new("/home/user/projects"));
        assert_eq!(
            user.dev_data_dir(),
            Path::new("/home/user/.apps/os.lazy.lazyrad/data")
        );
        assert_eq!(
            Home::from_var(None).config_dir(),
            Path::new("/transient/lazyrad/.apps/os.lazy.lazyrad/config")
        );
    }

    #[test]
    fn the_dialog_starts_in_the_first_existing_place() {
        let user = home("/home/user");
        assert_eq!(start_dir(&user, |_| true), Path::new("/home/user/projects"));
        assert_eq!(
            start_dir(&user, |p| p == Path::new("/home/user")),
            Path::new("/home/user")
        );
        assert_eq!(
            start_dir(&user, |p| p == Path::new(fhs::mount::TRANSIENT)),
            Path::new(fhs::mount::TRANSIENT)
        );
        assert_eq!(start_dir(&user, |_| false), Path::new("/"));
    }

    #[test]
    fn the_ide_platform_offers_an_in_window_filesystem() {
        let platform = LazyOsPlatform::ide(home("/home/user"), Path::new(IDE_EXE));
        let fs = platform.file_system().expect("LazyOS has painted dialogs");
        // The root lists the mount points the VFS omits, through the wrapper.
        let _ = fs.list(Path::new("/"));
    }

    #[test]
    fn packaged_apps_declare_the_services_their_scripts_call() {
        let found =
            LazyOsPlatform::ide(home("/home/user"), Path::new(IDE_EXE)).script_permissions(&[
                "fn form_load() { label.text = sys::confd::get(\"sys/ui/theme\").str_value; }",
                "fn watch() { sys::confd::on_changed(|e| ()); }",
            ]);
        assert_eq!(found.interfaces, ["os.lazy.confd.v1", PLAYER_INPUT]);
        assert_eq!(found.topics, ["subscribe:system/confd/changed/#"]);
        // A project that calls nothing still gets the player's keyboard.
        let none =
            LazyOsPlatform::ide(home("/home/user"), Path::new(IDE_EXE)).script_permissions(&[]);
        assert_eq!(none.interfaces, [PLAYER_INPUT]);
    }

    #[test]
    fn packaged_apps_that_play_songs_declare_the_mixer() {
        let found =
            LazyOsPlatform::ide(home("/home/user"), Path::new(IDE_EXE)).script_permissions(&[
                "fn go() { let d = modplay::play(modplay::decode(SONG)); }",
                "fn theme() { sys::confd::get(\"sys/ui/theme\") }",
            ]);
        assert_eq!(
            found.interfaces,
            ["os.lazy.audio.v1", "os.lazy.confd.v1", PLAYER_INPUT]
        );
    }

    #[test]
    fn the_platform_reports_lazyos_places() {
        let platform = LazyOsPlatform::ide(home("/home/user"), Path::new(IDE_EXE));
        assert_eq!(platform.name(), "lazyos");
        assert_eq!(
            platform.config_dir(),
            Some(PathBuf::from("/home/user/.apps/os.lazy.lazyrad/config"))
        );
        assert_eq!(
            platform.player_executable(),
            Some(PathBuf::from(
                "/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lrplay.elf"
            ))
        );
        assert!(!platform.default_monospace_font().is_empty());
        assert!(platform.documents().is_empty());
    }

    #[test]
    fn a_player_reports_the_documents_it_was_started_with() {
        let user = home("/home/user");
        let spec = PolicySpec::new(user.dev_data_dir());
        let picture = PathBuf::from("/home/user/Pictures/a.png");
        let platform = LazyOsPlatform::player(spec, user, vec![picture.clone()]);
        assert_eq!(platform.documents(), [picture]);
        assert!(matches!(platform.fs_policy(), FsPolicy::Sandboxed(_)));
    }
}

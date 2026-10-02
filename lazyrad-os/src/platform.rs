//! The [`Platform`] LazyOS installs into the LazyRAD runtime.
//!
//! It answers four questions the portable crates cannot:
//!
//! * which files a script may touch (decision D5: an app's private data
//!   directory, its own project read-only, nothing else);
//! * where LazyRAD keeps settings and projects (the user's home, [`Home`]);
//! * where the player binary is (`/system/bin/lrplay`);
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

use lazyrad_runtime::platform::Platform;
use lazyrad_runtime::{Access, FsPolicy, Sandbox};
use xui_core::widget::StdFileSystem;

/// Where installed apps live (`docs/packages.md`: `install_dir` is relative to
/// this). Read only: `pkgd` writes it.
pub const APPS_ROOT: &str = fhs::state::APPS_ROOT;

/// The player the image ships in `/system/bin`.
pub const PLAYER_PATH: &str = fhs::bin::LRPLAY;

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

    /// The IDE's settings, `<home>/.apps/lazyrad/config`.
    pub fn config_dir(&self) -> PathBuf {
        self.app_data(fhs::state::LAZYRAD_APP)
            .join(fhs::state::LAZYRAD_CONFIG)
    }

    /// Where the IDE keeps projects, `<home>/projects`.
    pub fn projects_dir(&self) -> PathBuf {
        self.path.join(fhs::state::LAZYRAD_PROJECTS)
    }

    /// The read/write directory of a project that is not an installed app,
    /// `<home>/.apps/lazyrad/data`.
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

/// The app id of `exe` when it runs from an installed package
/// (`/apps/<id>/<version>-<hash>/bin/<elf>`), else `None`.
pub fn installed_app_id(exe: &Path) -> Option<String> {
    let text = exe.to_string_lossy().replace('\\', "/");
    let rest = text.strip_prefix(APPS_ROOT)?.strip_prefix('/')?;
    let id = rest.split('/').next()?;
    let valid = !id.is_empty() && id != "." && id != "..";
    valid.then(|| id.to_owned())
}

/// The read/write root for a script: an installed app's
/// `<home>/.apps/<system_name>`, else `<home>/.apps/lazyrad/data`.
pub fn data_root(exe: &Path, home: &Home) -> PathBuf {
    match installed_app_id(exe) {
        Some(id) => home.app_data(&id),
        None => home.dev_data_dir(),
    }
}

/// The policy for a player running `project` from `exe`: read/write under
/// [`data_root`], plus read-only access to the project itself.
pub fn player_policy(exe: &Path, project: &Path, home: &Home) -> FsPolicy {
    let root = data_root(exe, home);
    FsPolicy::Sandboxed(Sandbox::new(root).allow(project.to_path_buf(), Access::Read))
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
    policy: FsPolicy,
    home: Home,
}

impl LazyOsPlatform {
    /// The platform for a player whose scripts run under `policy`.
    pub fn player(policy: FsPolicy, home: Home) -> LazyOsPlatform {
        LazyOsPlatform { policy, home }
    }

    /// The platform for the IDE: scripts (designer preview) may use the
    /// projects folder only.
    pub fn ide(home: Home) -> LazyOsPlatform {
        LazyOsPlatform {
            policy: FsPolicy::Sandboxed(Sandbox::new(home.projects_dir())),
            home,
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
        self.policy.clone()
    }

    fn player_executable(&self) -> Option<PathBuf> {
        Some(PathBuf::from(PLAYER_PATH))
    }

    /// The portable dialog over the shim's `std::fs`; the VFS lists mount
    /// points in `/` itself.
    fn file_system(&self) -> Option<Rc<dyn xui_core::widget::FileSystem>> {
        Some(Rc::new(StdFileSystem))
    }

    fn projects_dir(&self) -> PathBuf {
        start_dir(&self.home, |path| path.is_dir())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_EXE: &str = "/apps/user.me.todo/1.0.0-abcd1234/bin/lrplay.elf";

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
            Path::new("/home/user/.apps/lazyrad/config")
        );
        assert_eq!(user.projects_dir(), Path::new("/home/user/projects"));
        assert_eq!(
            user.dev_data_dir(),
            Path::new("/home/user/.apps/lazyrad/data")
        );
        assert_eq!(
            Home::from_var(None).config_dir(),
            Path::new("/transient/lazyrad/.apps/lazyrad/config")
        );
    }

    #[test]
    fn an_installed_app_writes_its_own_folder_in_the_home() {
        let exe = Path::new(APP_EXE);
        assert_eq!(installed_app_id(exe).as_deref(), Some("user.me.todo"));
        assert_eq!(
            data_root(exe, &home("/home/user")),
            Path::new("/home/user/.apps/user.me.todo")
        );
        assert!(!data_root(exe, &home("/home/user")).starts_with(APPS_ROOT));
    }

    #[test]
    fn a_dev_run_uses_lazyrads_own_data_folder() {
        let exe = Path::new(PLAYER_PATH);
        assert_eq!(installed_app_id(exe), None);
        assert_eq!(
            data_root(exe, &home("/home/admin")),
            Path::new("/home/admin/.apps/lazyrad/data")
        );
        assert_eq!(
            data_root(exe, &Home::from_var(None)),
            Path::new("/transient/lazyrad/.apps/lazyrad/data")
        );
    }

    #[test]
    fn odd_paths_are_not_app_ids() {
        for bad in [
            "/apps/",
            "/apps//x/bin/a",
            "/apps/../bin/a",
            "/appsx/y/bin/a",
        ] {
            assert_eq!(installed_app_id(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn the_player_policy_is_private_data_plus_a_read_only_project() {
        // A directory grant needs the directory to exist, so use a real one.
        let project =
            std::env::temp_dir().join(format!("lazyrad-os-policy-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let file = project.join("main.lfm");
        std::fs::write(&file, "x").unwrap();
        let file = file.to_string_lossy().into_owned();
        let user = home("/home/user");

        let policy = player_policy(Path::new(APP_EXE), &project, &user);
        let own = policy
            .resolve("notes.txt", Access::Write)
            .expect("own data");
        let own = own.to_string_lossy().replace('\\', "/");
        assert!(
            own.ends_with("/home/user/.apps/user.me.todo/notes.txt"),
            "{own}"
        );
        assert!(
            policy.resolve(&file, Access::Read).is_ok(),
            "the project is readable"
        );
        assert!(
            policy.resolve(&file, Access::Write).is_err(),
            "the project is read-only"
        );
        assert!(policy.resolve("/system/etc/passwd", Access::Read).is_err());
        assert!(policy.resolve("../other/x", Access::Read).is_err());
        let _ = std::fs::remove_dir_all(&project);
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
        let platform = LazyOsPlatform::ide(home("/home/user"));
        let fs = platform.file_system().expect("LazyOS has painted dialogs");
        // The root lists the mount points the VFS omits, through the wrapper.
        let _ = fs.list(Path::new("/"));
    }

    #[test]
    fn the_platform_reports_lazyos_places() {
        let platform = LazyOsPlatform::ide(home("/home/user"));
        assert_eq!(platform.name(), "lazyos");
        assert_eq!(
            platform.config_dir(),
            Some(PathBuf::from("/home/user/.apps/lazyrad/config"))
        );
        assert_eq!(
            platform.player_executable(),
            Some(PathBuf::from(PLAYER_PATH))
        );
        assert!(!platform.default_monospace_font().is_empty());
    }
}

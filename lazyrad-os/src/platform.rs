//! The [`Platform`] LazyOS installs into the LazyRAD runtime.
//!
//! It answers four questions the portable crates cannot:
//!
//! * which files a script may touch (decision D5: an app's private data
//!   directory, its own project read-only, nothing else);
//! * where LazyRAD keeps settings (`/data/config/lazyrad`);
//! * where the player binary is (`/LRPLAY.ELF` on the boot volume);
//! * the monospace face (the bundled JetBrains Mono).
//!
//! Dialogs are left at the trait's "cancel" default here: LazyOS dialogs are
//! painted in-window (xui's portable `FileDialog`), so the IDE drives them
//! through its own widgets instead of a blocking call (see `bin/lazyrad.rs`).

use std::path::{Path, PathBuf};

use lazyrad_runtime::platform::Platform;
use lazyrad_runtime::{Access, FsPolicy, Sandbox};

/// Where installed apps live (`docs/packages.md`: `install_dir` is relative to
/// this).
pub const APPS_ROOT: &str = "/data/apps";

/// The player on the boot volume (`LRPLAY.ELF`, an 8.3 name).
pub const PLAYER_PATH: &str = "/LRPLAY.ELF";

/// The IDE's settings directory.
pub const CONFIG_DIR: &str = "/data/config/lazyrad";

/// Where the IDE keeps projects by default.
pub const PROJECTS_DIR: &str = "/data/projects";

/// Scratch space for a player that is not an installed app and has no `/data`.
pub const SCRATCH_DIR: &str = "/tmp/lazyrad";

/// The app id of `exe` when it runs from an installed package
/// (`/data/apps/<id>/<version>-<hash>/bin/<elf>`), else `None`.
pub fn installed_app_id(exe: &Path) -> Option<String> {
    let text = exe.to_string_lossy().replace('\\', "/");
    let rest = text.strip_prefix(APPS_ROOT)?.strip_prefix('/')?;
    let id = rest.split('/').next()?;
    let valid = !id.is_empty() && id != "." && id != "..";
    valid.then(|| id.to_owned())
}

/// The read/write root for a script: an installed app's private `data/`
/// directory, else `/data/lazyrad-data` when a `/data` volume exists, else
/// scratch space under `/tmp`.
pub fn data_root(exe: &Path, data_volume_present: bool) -> PathBuf {
    match installed_app_id(exe) {
        Some(id) => Path::new(APPS_ROOT).join(id).join("data"),
        None if data_volume_present => PathBuf::from("/data/lazyrad-data"),
        None => PathBuf::from(SCRATCH_DIR),
    }
}

/// The policy for a player running `project` from `exe`: read/write under
/// [`data_root`], plus read-only access to the project itself.
pub fn player_policy(exe: &Path, project: &Path, data_volume_present: bool) -> FsPolicy {
    let root = data_root(exe, data_volume_present);
    FsPolicy::Sandboxed(Sandbox::new(root).allow(project.to_path_buf(), Access::Read))
}

/// What the player and the IDE share on LazyOS.
pub struct LazyOsPlatform {
    policy: FsPolicy,
}

impl LazyOsPlatform {
    /// The platform for a player whose scripts run under `policy`.
    pub fn player(policy: FsPolicy) -> LazyOsPlatform {
        LazyOsPlatform { policy }
    }

    /// The platform for the IDE: scripts (designer preview) may use
    /// [`PROJECTS_DIR`] only.
    pub fn ide() -> LazyOsPlatform {
        LazyOsPlatform {
            policy: FsPolicy::Sandboxed(Sandbox::new(PathBuf::from(PROJECTS_DIR))),
        }
    }
}

impl Platform for LazyOsPlatform {
    fn name(&self) -> &'static str {
        "lazyos"
    }

    fn config_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from(CONFIG_DIR))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_EXE: &str = "/data/apps/user.me.todo/1.0.0-abcd1234/bin/lrplay.elf";

    #[test]
    fn an_installed_app_has_a_private_data_root() {
        let exe = Path::new(APP_EXE);
        assert_eq!(installed_app_id(exe).as_deref(), Some("user.me.todo"));
        assert_eq!(
            data_root(exe, true),
            Path::new("/data/apps/user.me.todo/data")
        );
    }

    #[test]
    fn a_dev_run_uses_data_when_mounted_and_tmp_otherwise() {
        let exe = Path::new("/LRPLAY.ELF");
        assert_eq!(installed_app_id(exe), None);
        assert_eq!(data_root(exe, true), Path::new("/data/lazyrad-data"));
        assert_eq!(data_root(exe, false), Path::new(SCRATCH_DIR));
    }

    #[test]
    fn odd_paths_are_not_app_ids() {
        for bad in [
            "/data/apps/",
            "/data/apps//x/bin/a",
            "/data/apps/../bin/a",
            "/data/appsx/y/bin/a",
        ] {
            assert_eq!(installed_app_id(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn the_player_policy_is_private_data_plus_a_read_only_project() {
        let exe = Path::new(APP_EXE);
        let project = Path::new("/data/apps/user.me.todo/1.0.0-abcd1234/resources/project");
        let policy = player_policy(exe, project, true);
        let own = policy
            .resolve("notes.txt", Access::Write)
            .expect("own data");
        assert!(own.starts_with("/data/apps/user.me.todo/data"));
        let read = policy.resolve(
            "/data/apps/user.me.todo/1.0.0-abcd1234/resources/project/main.lfm",
            Access::Read,
        );
        assert!(read.is_ok(), "the project is readable");
        let write = policy.resolve(
            "/data/apps/user.me.todo/1.0.0-abcd1234/resources/project/main.lfm",
            Access::Write,
        );
        assert!(write.is_err(), "the project is read-only");
        assert!(policy.resolve("/etc/passwd", Access::Read).is_err());
        assert!(policy.resolve("../other/data/x", Access::Read).is_err());
    }

    #[test]
    fn the_platform_reports_lazyos_places() {
        let platform = LazyOsPlatform::ide();
        assert_eq!(platform.name(), "lazyos");
        assert_eq!(platform.config_dir(), Some(PathBuf::from(CONFIG_DIR)));
        assert_eq!(
            platform.player_executable(),
            Some(PathBuf::from(PLAYER_PATH))
        );
        assert!(!platform.default_monospace_font().is_empty());
    }
}

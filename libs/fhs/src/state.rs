//! Persistent state: configuration, installed apps, logs and home directories,
//! each at its place in the OS volume's tree (F4). Nothing new is written
//! under `/data`; the one `/data` path left, [`LEGACY_DATA_CONFD`], is read
//! once as a seed.

/// `confd`'s store on the OS volume, 0700 root: only `confd` reads the raw
/// store, everyone else goes through it. Written by `confd`.
pub const CONF_ROOT: &str = "/conf";

/// Where `confd` keeps its store when [`CONF_ROOT`] is not writable (a
/// recovery boot): the ramfs, so settings last until the next boot only and
/// `confd` reports itself degraded. Written by `confd`.
pub const CONF_FALLBACK: &str = "/transient/conf";

/// Per-service state that is not key/value, one `<service>/` directory each,
/// created by its owner (e.g. `keyd`'s verifiers, #447). 0700 root; `confd`
/// creates nothing there.
pub const CONF_SVC: &str = "/conf/svc";

/// The marker `confd` writes in [`CONF_ROOT`] once it has seeded the store
/// from [`LEGACY_DATA_CONFD`], so a setting deleted after the migration does
/// not come back. Written by `confd`.
pub const CONF_SEEDED_MARKER: &str = "/conf/.seeded-from-data";

/// The F0 to F3 `confd` store on the `/data` volume. Seed only: `confd` copies
/// it into [`CONF_ROOT`] once and never writes it. Removed in F7.
pub const LEGACY_DATA_CONFD: &str = "/data/confd";

/// `logd`'s persistent journals, one `<source>.log` (plus `.1`/`.2` rotations)
/// per source, on the OS volume. Written by `logd` and, for [`PKG_LOG_FILE`],
/// by `pkgd`.
pub const LOGS_ROOT: &str = "/logs";

/// `pkgd`'s hash-chained audit log. Written by `pkgd`, which caps it itself:
/// `logd`'s rotation and budget leave it alone.
pub const PKG_LOG_FILE: &str = "/logs/pkg.log";

/// Installed apps, one directory per `system_name` per version. Written only
/// by `pkgd`.
pub const APPS_ROOT: &str = "/apps";

/// Per-user home directories, `<HOME_ROOT>/<user>` (see
/// [`home_of`](crate::home_of)): the optional home volume, or the directories
/// of the OS volume without one. Written by the users and the apps they run.
pub const HOME_ROOT: &str = "/home";

/// Per-app data inside a home, `<home>/.apps/<system_name>/` (see
/// [`app_data_dir`](crate::app_data_dir)). Relative to a home. Written by the
/// app.
pub const APP_DATA_DIR: &str = ".apps";

/// The desktop folder inside a home, `<home>/Desktop`: LazyShell shows its
/// entries as the desktop's icons and seeds it with shortcuts the first time
/// it is missing. Relative to a home. Written by the user (and the shell's
/// seed).
pub const DESKTOP_DIR: &str = "Desktop";

/// The LazyRAD IDE's app data directory name inside a home: its `system_name`,
/// since it is an installed package (`os.lazy.lazyrad`, docs/lazyrad-package-plan.md).
/// Its settings and the data of projects run from the IDE live in
/// [`app_data_dir`](crate::app_data_dir)`(home, LAZYRAD_APP)`. Written by
/// `lazyrad` and `lrplay`.
pub const LAZYRAD_APP: &str = "os.lazy.lazyrad";

/// The directory name the IDE used before it was a package
/// (`<home>/.apps/lazyrad`). `lazyrad` moves it to [`LAZYRAD_APP`] once, at
/// start.
pub const LAZYRAD_LEGACY_APP: &str = "lazyrad";

/// `lazyrad`'s settings, relative to its app data directory
/// (`<home>/.apps/os.lazy.lazyrad/config`). Written by `lazyrad`.
pub const LAZYRAD_CONFIG: &str = "config";

/// The read/write directory of a project that is not an installed app,
/// relative to `lazyrad`'s app data directory (`<home>/.apps/os.lazy.lazyrad/data`).
/// An installed app writes its own `<home>/.apps/<system_name>/` instead.
/// Written by `lrplay`.
pub const LAZYRAD_DATA: &str = "data";

/// `lazyrad` projects, relative to a home (`<home>/projects`). Written by
/// `lazyrad`.
pub const LAZYRAD_PROJECTS: &str = "projects";

/// `lazyrad`'s home when `$HOME` is unset, on the ramfs: nothing is kept
/// across a reboot, and `lazyrad`/`lrplay` warn about it. Written by
/// `lazyrad`.
pub const LAZYRAD_TMP: &str = "/transient/lazyrad";

/// The resolvers `netd` learned (DHCP or manual), in `resolv.conf` syntax, on
/// the ramfs; Linux programs read it as
/// [`LINUX_RESOLV_CONF`](crate::etc::LINUX_RESOLV_CONF) (docs/tls-plan.md
/// §5.1). Written by `netd`.
pub const RESOLV_CONF: &str = "/transient/net/resolv.conf";

/// Doom's config and save directory when the player has no home (otherwise
/// its per-user folder, `$HOME/.apps/org.lazy.doom`). Written by the
/// `org.lazy.doom` package.
pub const DOOM_TMP: &str = "/tmp/doom";

/// The verdict line Doom's headless mode writes for a harness to read
/// (`doom/src/headless.rs`).
pub const DOOM_RESULT: &str = "/tmp/doom-result.txt";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::{DATA, HOME, TMP, TRANSIENT};

    #[test]
    fn state_lives_in_the_target_tree() {
        for dir in [CONF_ROOT, LOGS_ROOT, APPS_ROOT] {
            assert!(!dir.starts_with(DATA), "{dir}");
            assert_eq!(
                dir.matches('/').count(),
                1,
                "{dir} is a top-level directory"
            );
        }
        assert!(CONF_SVC.starts_with(CONF_ROOT));
        assert!(CONF_SEEDED_MARKER.starts_with(CONF_ROOT));
        assert!(PKG_LOG_FILE.starts_with(LOGS_ROOT));
        assert_eq!(HOME_ROOT, HOME);
        assert!(CONF_FALLBACK.starts_with(TRANSIENT));
        assert!(LAZYRAD_TMP.starts_with(TRANSIENT));
        assert!(RESOLV_CONF.starts_with(TRANSIENT));
        for name in [
            APP_DATA_DIR,
            LAZYRAD_APP,
            LAZYRAD_CONFIG,
            LAZYRAD_DATA,
            LAZYRAD_PROJECTS,
        ] {
            assert!(!name.is_empty() && !name.contains('/'), "{name}");
        }
    }

    #[test]
    fn the_legacy_confd_store_is_the_only_seed() {
        assert!(LEGACY_DATA_CONFD.starts_with(DATA));
    }

    #[test]
    fn doom_scratch_lives_on_the_ramfs() {
        assert!(DOOM_TMP.starts_with(TMP));
        assert!(DOOM_RESULT.starts_with(TMP));
    }
}

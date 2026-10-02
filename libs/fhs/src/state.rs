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

/// `lazyrad`'s settings. Written by `lazyrad`. Target (F4, lazyrad step):
/// `"$HOME/.apps/lazyrad/config"`.
pub const LAZYRAD_CONFIG: &str = "/data/config/lazyrad";

/// `lazyrad` projects. Written by `lazyrad`. Target (F4, lazyrad step):
/// `"$HOME/projects"`.
pub const LAZYRAD_PROJECTS: &str = "/data/projects";

/// `lazyrad`'s data directory. Written by `lazyrad`. Target (F4, lazyrad
/// step): `"$HOME/.apps/lazyrad/data"`.
pub const LAZYRAD_DATA: &str = "/data/lazyrad-data";

/// `lazyrad`'s scratch directory on the ramfs. Written by `lazyrad`.
pub const LAZYRAD_TMP: &str = "/transient/lazyrad";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::{DATA, HOME, TRANSIENT};

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
        assert!(!APP_DATA_DIR.contains('/'));
    }

    #[test]
    fn the_legacy_confd_store_is_the_only_seed() {
        assert!(LEGACY_DATA_CONFD.starts_with(DATA));
    }
}

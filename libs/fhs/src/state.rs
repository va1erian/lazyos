//! Persistent state: configuration, installed apps, logs, home directories and
//! `lazyrad`'s data. All of it lives on the `/data` volume today.

/// Where `confd` keeps its store, best first; the last entry is the ramfs
/// fallback used when no persistent location is writable. `/data/confd` needs
/// the data disk; `/system/confd` is the planned home on a writable system
/// volume. Written by `confd`. Target (F4): `"/conf"`, with
/// `"/transient/conf"` as the degraded fallback.
pub const CONFD_DIRS: [&str; 3] = ["/data/confd", "/system/confd", "/tmp/confd"];

/// Installed apps, one directory per `system_name` per version. Written by
/// `pkgd`. Target (F4): `"/apps"`.
pub const APPS_ROOT: &str = "/data/apps";

/// `pkgd`'s log directory. Written by `pkgd`. Target (F4): `"/logs"`.
pub const PKG_LOG_DIR: &str = "/data/log";

/// `pkgd`'s hash-chained audit log. Written by `pkgd`. Target (F4):
/// `"/logs/pkg.log"`.
pub const PKG_LOG_FILE: &str = "/data/log/pkg.log";

/// Per-user home directories, `<HOME_ROOT>/<user>`. Written by the users and
/// the apps they run. Target (F1): `"/home"` on its own volume.
pub const HOME_ROOT: &str = "/data/home";

/// `lazyrad`'s settings. Written by `lazyrad`. Target (F4):
/// `"/home/<user>/.apps/..."`.
pub const LAZYRAD_CONFIG: &str = "/data/config/lazyrad";

/// `lazyrad` projects. Written by `lazyrad`. Target (F4): under the user's
/// home.
pub const LAZYRAD_PROJECTS: &str = "/data/projects";

/// `lazyrad`'s data directory. Written by `lazyrad`. Target (F4): under the
/// user's home.
pub const LAZYRAD_DATA: &str = "/data/lazyrad-data";

/// `lazyrad`'s scratch directory on the ramfs. Written by `lazyrad`. Target
/// (F1): under `"/transient"`.
pub const LAZYRAD_TMP: &str = "/tmp/lazyrad";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::{DATA, TMP};

    #[test]
    fn state_lives_under_its_volume() {
        for dir in [
            APPS_ROOT,
            PKG_LOG_DIR,
            HOME_ROOT,
            LAZYRAD_CONFIG,
            LAZYRAD_PROJECTS,
            LAZYRAD_DATA,
        ] {
            assert!(dir.starts_with(DATA), "{dir}");
        }
        assert!(PKG_LOG_FILE.starts_with(PKG_LOG_DIR));
        assert!(CONFD_DIRS[0].starts_with(DATA));
        assert!(CONFD_DIRS[2].starts_with(TMP));
        assert!(LAZYRAD_TMP.starts_with(TMP));
    }
}

//! Where installed apps live and how an archive maps onto the disk.
//!
//! `pkgd` writes as root, so a path it composes must be right even for a
//! package `lazypkg` already validated: every component is re-checked here
//! before it reaches a syscall, and nothing is ever joined from a string that
//! could climb out of `/data/apps`. (The ext2 volume has no symlinks, so a
//! lexically safe path is a physically safe one.)

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The data volume's mount point.
pub const DATA_ROOT: &str = fhs::mount::DATA;
/// Installed applications, one directory per `system_name` per version.
pub const APPS_ROOT: &str = fhs::state::APPS_ROOT;
/// Where `pkgd` keeps its audit log.
pub const LOG_DIR: &str = fhs::state::PKG_LOG_DIR;
/// The hash-chained audit log (`crate::audit`).
pub const LOG_FILE: &str = fhs::state::PKG_LOG_FILE;
/// The `confd` subtree holding one record per installed app.
pub const CONFD_PREFIX: &str = "sys/apps";
/// The stored manifest inside an install directory.
pub const MANIFEST_FILE: &str = "manifest.toml";
/// Hex digits of the digest in an install directory name.
const DIGEST_CHARS: usize = 8;

/// Why a path component was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// Not a valid `system_name`.
    SystemName,
    /// Not `<version>-<8 hex>`, or not a valid version.
    InstallDir,
    /// An archive entry name that could escape its directory.
    Entry,
}

impl core::fmt::Display for PathError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            PathError::SystemName => "the application name is not a valid reverse-DNS name",
            PathError::InstallDir => "the install directory name is malformed",
            PathError::Entry => "a package entry name is not a safe relative path",
        })
    }
}

/// The `system_name` grammar (`docs/packages.md`): lowercase letters, digits
/// and `-` in labels separated by `.`, at least three labels, at most 128
/// bytes. Also the alphabet `confd` accepts in a path segment.
pub fn valid_system_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    let mut labels = 0;
    for label in name.split('.') {
        labels += 1;
        let bytes_ok = label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') || !bytes_ok {
            return false;
        }
    }
    labels >= 3
}

/// `<system_name>/<version>-<8 lowercase hex>`, the shape `lazypkg` builds.
pub fn valid_install_dir(install_dir: &str) -> bool {
    let Some((system_name, leaf)) = install_dir.split_once('/') else {
        return false;
    };
    let Some((version, digest)) = leaf.rsplit_once('-') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    valid_system_name(system_name)
        && parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 5 && part.bytes().all(|b| b.is_ascii_digit())
        })
        && digest.len() == DIGEST_CHARS
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `/data/apps/<system_name>`: every version of one app.
pub fn app_dir(system_name: &str) -> Result<String, PathError> {
    if !valid_system_name(system_name) {
        return Err(PathError::SystemName);
    }
    Ok(format!("{APPS_ROOT}/{system_name}"))
}

/// `/data/apps/<install_dir>`.
pub fn install_path(install_dir: &str) -> Result<String, PathError> {
    if !valid_install_dir(install_dir) {
        return Err(PathError::InstallDir);
    }
    Ok(format!("{APPS_ROOT}/{install_dir}"))
}

/// The `confd` key of an installed app: `sys/apps/<system_name>`.
pub fn confd_key(system_name: &str) -> Result<String, PathError> {
    if !valid_system_name(system_name) {
        return Err(PathError::SystemName);
    }
    Ok(format!("{CONFD_PREFIX}/{system_name}"))
}

/// The `system_name` a `sys/apps/<system_name>` key names, if it is one.
pub fn system_name_of_key(key: &str) -> Option<&str> {
    let name = key.strip_prefix(CONFD_PREFIX)?.strip_prefix('/')?;
    valid_system_name(name).then_some(name)
}

/// Whether `entry` is a relative `/`-separated path with no empty, `.` or `..`
/// component, no backslash, NUL or control character, at most 255 bytes: the
/// check `lazypkg` makes at open, repeated here because this is the last stop
/// before a root `write_file`.
pub fn safe_entry(entry: &str) -> bool {
    let name = entry.strip_suffix('/').unwrap_or(entry);
    !name.is_empty()
        && entry.len() <= 255
        && !entry.starts_with('/')
        && !entry.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'\\')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !name.contains(':')
}

/// `<install_path>/<entry>`, refusing an unsafe entry name.
pub fn entry_path(install_path: &str, entry: &str) -> Result<String, PathError> {
    if !safe_entry(entry) {
        return Err(PathError::Entry);
    }
    Ok(format!(
        "{}/{}",
        install_path.trim_end_matches('/'),
        entry.trim_end_matches('/')
    ))
}

/// The package directory whose files are programs.
pub const BIN_DIR: &str = "bin";
/// Permissions of an extracted program (anything under [`BIN_DIR`]).
pub const EXEC_MODE: u16 = 0o755;
/// Permissions of every other extracted file.
pub const DATA_MODE: u16 = 0o644;

/// The permission bits `pkgd` gives the extracted file `entry` (an archive
/// file name, `bin/app.elf`): [`EXEC_MODE`] for a file under the package's
/// top-level `bin/` directory, because native spawn needs an `x` bit (root
/// included), and [`DATA_MODE`] for everything else, so an icon or resource
/// can never be started as a program.
pub fn file_mode(entry: &str) -> u16 {
    match entry.split_once('/') {
        Some((top, rest)) if top == BIN_DIR && !rest.is_empty() => EXEC_MODE,
        _ => DATA_MODE,
    }
}

/// Every directory the archive needs, parents before children, without
/// duplicates: each explicit directory entry and every ancestor of every file.
/// Entry names are the archive's (`dir/` for a directory, `dir/file` for a
/// file). An unsafe name is an error, so the plan never holds one.
pub fn directories<'a>(
    entries: impl Iterator<Item = (&'a str, bool)>,
) -> Result<Vec<String>, PathError> {
    let mut dirs: Vec<String> = Vec::new();
    for (name, is_dir) in entries {
        if !safe_entry(name) {
            return Err(PathError::Entry);
        }
        let trimmed = name.trim_end_matches('/');
        let mut upto = 0;
        let parts: Vec<&str> = trimmed.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            upto += part.len() + usize::from(index > 0);
            if last && !is_dir {
                break;
            }
            let dir = &trimmed[..upto];
            if !dirs.iter().any(|known| known == dir) {
                dirs.push(String::from(dir));
            }
        }
    }
    // Shallow first, so a parent always exists before its child.
    dirs.sort_by_key(|dir| dir.matches('/').count());
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_names_follow_the_package_grammar() {
        for good in [
            "org.lazy.counter",
            "a.b.c",
            "org.lazy.my-app2",
            "com.example.notes",
        ] {
            assert!(valid_system_name(good), "{good}");
        }
        for bad in [
            "",
            "org.lazy",
            "Org.Lazy.Counter",
            "org..counter",
            "org.lazy.-x",
            "org.lazy.x-",
            "../x.y.z",
            "org.lazy.a/b",
            "org.lazy.a b",
            &"a.".repeat(70),
        ] {
            assert!(!valid_system_name(bad), "{bad}");
        }
    }

    #[test]
    fn install_dirs_have_the_lazypkg_shape() {
        assert!(valid_install_dir("org.lazy.counter/1.0.0-0a1b2c3d"));
        for bad in [
            "org.lazy.counter",
            "org.lazy.counter/1.0.0",
            "org.lazy.counter/1.0.0-0A1B2C3D",
            "org.lazy.counter/1.0.0-0a1b2c3",
            "org.lazy.counter/1.0-0a1b2c3d",
            "org.lazy.counter/../x-0a1b2c3d",
            "org.lazy.counter/1.0.0-0a1b2c3d/x",
            "../1.0.0-0a1b2c3d",
            "/org.lazy.counter/1.0.0-0a1b2c3d",
        ] {
            assert!(!valid_install_dir(bad), "{bad}");
        }
    }

    #[test]
    fn paths_are_built_only_from_valid_parts() {
        assert_eq!(
            install_path("org.lazy.counter/1.0.0-0a1b2c3d").unwrap(),
            "/data/apps/org.lazy.counter/1.0.0-0a1b2c3d"
        );
        assert_eq!(install_path("../../etc"), Err(PathError::InstallDir));
        assert_eq!(
            app_dir("org.lazy.counter").unwrap(),
            "/data/apps/org.lazy.counter"
        );
        assert_eq!(app_dir("bad"), Err(PathError::SystemName));
        assert_eq!(
            confd_key("org.lazy.counter").unwrap(),
            "sys/apps/org.lazy.counter"
        );
        assert_eq!(confd_key("x/y"), Err(PathError::SystemName));
        assert_eq!(
            system_name_of_key("sys/apps/org.lazy.counter"),
            Some("org.lazy.counter")
        );
        assert_eq!(system_name_of_key("sys/apps/org.lazy.counter/x"), None);
        assert_eq!(system_name_of_key("sys/ui/menu"), None);
    }

    #[test]
    fn entry_names_that_could_escape_are_refused() {
        assert!(safe_entry("bin/app.elf"));
        assert!(safe_entry("resources/a b/c.txt"));
        assert!(safe_entry("docs/"));
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "bin/../../x",
            "bin//app",
            "./x",
            "bin/./x",
            "a\\b",
            "a\0b",
            "a\nb",
            "C:/x",
            &"a".repeat(256),
        ] {
            assert!(!safe_entry(bad), "{bad:?}");
        }
        assert_eq!(
            entry_path("/data/apps/x.y.z/1.0.0-00000000", "bin/app.elf").unwrap(),
            "/data/apps/x.y.z/1.0.0-00000000/bin/app.elf"
        );
        assert_eq!(entry_path("/data/apps/x", "../y"), Err(PathError::Entry));
    }

    #[test]
    fn the_directory_plan_lists_parents_first_without_duplicates() {
        let entries = [
            ("manifest.toml", false),
            ("bin/", true),
            ("bin/counter.elf", false),
            ("icons/app-16.png", false),
            ("resources/a/b/c.txt", false),
            ("resources/a/d.txt", false),
        ];
        let dirs = directories(entries.iter().copied()).unwrap();
        assert_eq!(
            dirs,
            ["bin", "icons", "resources", "resources/a", "resources/a/b"]
        );
        // An explicit directory entry with nothing in it still gets created.
        let empty = directories([("resources/empty/", true)].into_iter()).unwrap();
        assert_eq!(empty, ["resources", "resources/empty"]);
        // A hostile name stops the plan.
        assert_eq!(
            directories([("bin/../x", false)].into_iter()),
            Err(PathError::Entry)
        );
    }

    #[test]
    fn only_files_under_bin_are_executable() {
        for program in ["bin/app.elf", "bin/tools/helper.elf"] {
            assert_eq!(file_mode(program), EXEC_MODE, "{program}");
        }
        for data in [
            "manifest.toml",
            "icons/app-16.png",
            "resources/bin/x",
            "docs/bin.md",
            "bin",
            "bin/",
            "binx/app.elf",
            "Bin/app.elf",
        ] {
            assert_eq!(file_mode(data), DATA_MODE, "{data}");
        }
    }
}

//! Who may ask `pkgd` for what, and which package files it will read for them.
//!
//! `pkgd` runs as root so it can write `/apps` and load kernel policy. That
//! makes it a confused deputy for the paths it is handed: reading a file *as
//! root* on a user's say-so would let that user install (and so copy out, into
//! world-readable `/apps`) a package they could not read themselves. There is
//! no "open as uid" call, so [`source_allowed`] confines an unprivileged caller
//! to locations that are readable by design: the shared `/transient`, the
//! image's public data in `/system/share` (its sample packages) and the
//! caller's own home directory. A system service may name any absolute path.
//! Privilege is a capability, never a uid (issue #623): a root *login session*
//! holds none and is confined like any other user. The path is
//! normalised first (`//`, `.` and `..` folded; the volume has no symlinks, so
//! that is the file that will be read) and `pkgd` reads the normalised path.

use alloc::string::String;
use alloc::vec::Vec;

/// Longest source path accepted (the native path calls take at most 1024).
pub const MAX_PATH: usize = 1024;

/// What the kernel stamped on the task that sent a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    /// Login session id; `0` for a system service.
    pub session: u64,
    /// Policy label id; non-zero for a sandboxed application.
    pub label_id: u32,
    /// The caller is a system service: it holds `CAP_SETUID`, the authority
    /// `init` keeps for the services it starts and never stamps on a login
    /// session.
    pub system: bool,
}

/// Whether `caller` may install or remove applications: a system service, or
/// the owner of a login session. A sandboxed application never may, however it is labelled
/// (the kernel's default-deny already stops it; this is the second lock).
pub fn may_manage(caller: &Caller) -> Result<(), &'static str> {
    if caller.label_id != 0 {
        return Err("applications may not install or remove other applications");
    }
    if caller.system || caller.session != 0 {
        Ok(())
    } else {
        Err("only a logged-in user or a system service may install or remove applications")
    }
}

/// Whether `caller` may ask about packages at all (`Inspect`, `List`): anything
/// but a sandboxed application.
pub fn may_inspect(caller: &Caller) -> Result<(), &'static str> {
    if caller.label_id != 0 {
        Err("applications may not inspect packages")
    } else {
        Ok(())
    }
}

/// `path` is an absolute path of sane shape: no NUL or control character, no
/// `.` or `..` or empty component, at most [`MAX_PATH`] bytes.
pub fn well_formed(path: &str) -> bool {
    path.len() <= MAX_PATH
        && path.starts_with('/')
        && path.len() > 1
        && !path.bytes().any(|b| b < 0x20 || b == 0x7f)
        && path[1..]
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The shared place any user may install from: the `/transient` ramfs.
pub const SHARED_SOURCE: &str = fhs::mount::TRANSIENT;
/// The image's public data, world-readable by construction (the build writes
/// it 0644 under 0755 directories): its sample packages are meant for users.
pub const SYSTEM_SOURCE: &str = fhs::SYSTEM_SHARE;

/// The refusal an unprivileged caller gets for any other place.
pub const SOURCE_RULE: &str =
    "packages can only be installed from /transient, /system/share or your home folder";

/// `path` folded lexically: repeated `/` and `.` components dropped, `..`
/// removing the component before it. `None` when it is not absolute, climbs
/// above `/`, holds a control character or the result is not
/// [`well_formed`]. ext2 has no symlinks, so this is the file a read opens.
pub fn normalise(path: &str) -> Option<String> {
    if !path.starts_with('/')
        || path.len() > MAX_PATH
        || path.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    let mut out = String::with_capacity(path.len());
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    well_formed(&out).then_some(out)
}

/// Whether `path` lies strictly under `root` (a directory path without a
/// trailing slash), component-wise: `/transient/x` is under `/transient`,
/// `/transientx` is not, and neither is `/transient` itself.
pub fn under(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// Whether `pkgd` will read the package at `path` for `caller`, and the
/// normalised path to read:
///
/// ```text
/// allowed = under(path, /transient) || under(path, /system/share)
///        || under(path, caller_home) || caller.system
/// ```
///
/// `home` is the caller's home directory from the account database, when
/// known; a malformed one grants nothing.
pub fn source_allowed(caller: &Caller, home: Option<&str>, path: &str) -> Result<String, String> {
    let Some(path) = normalise(path) else {
        return Err(String::from("that is not a valid absolute file path"));
    };
    let own_home = home.is_some_and(|home| well_formed(home) && under(&path, home));
    if caller.system || under(&path, SHARED_SOURCE) || under(&path, SYSTEM_SOURCE) || own_home {
        Ok(path)
    } else {
        Err(String::from(SOURCE_RULE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: Caller = Caller {
        uid: 0,
        session: 0,
        label_id: 0,
        system: true,
    };
    /// A root login session: uid 0, no capability.
    const ROOT_SESSION: Caller = Caller {
        uid: 0,
        session: 9,
        label_id: 0,
        system: false,
    };
    const DAEMON: Caller = Caller {
        uid: 901,
        session: 0,
        label_id: 0,
        system: false,
    };
    const SANDBOXED: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 7,
        system: false,
    };

    #[test]
    fn root_and_session_owners_manage_apps() {
        assert!(may_manage(&ROOT).is_ok());
        assert!(may_manage(&USER).is_ok());
        assert!(may_manage(&DAEMON).is_err());
        // A uid alone is nothing: a sessionless root task without the
        // capability may not manage, a root login session may like any user.
        let bare_root = Caller {
            system: false,
            ..ROOT
        };
        assert!(may_manage(&bare_root).is_err());
        assert!(may_manage(&ROOT_SESSION).is_ok());
        let sandboxed_root = Caller {
            uid: 0,
            system: true,
            ..SANDBOXED
        };
        assert!(may_manage(&SANDBOXED).is_err());
        assert!(may_manage(&sandboxed_root).is_err());
    }

    #[test]
    fn a_sandboxed_app_may_not_even_inspect() {
        assert!(may_inspect(&USER).is_ok());
        assert!(may_inspect(&DAEMON).is_ok());
        assert!(may_inspect(&SANDBOXED).is_err());
    }

    #[test]
    fn well_formed_paths() {
        for good in ["/transient/a.lzp", "/home/user/a b.lzp"] {
            assert!(well_formed(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "relative.lzp",
            "/transient/../etc/x",
            "/transient/./x",
            "/transient//x",
            "/transient/x\0",
            "/transient/x\n",
            &format!("/{}", "a".repeat(MAX_PATH)),
        ] {
            assert!(!well_formed(bad), "{bad:?}");
        }
    }

    #[test]
    fn normalising_folds_slashes_dots_and_parents() {
        assert_eq!(
            normalise("/transient//a/./b.lzp").as_deref(),
            Some("/transient/a/b.lzp")
        );
        assert_eq!(
            normalise("/home/user/../admin/x.lzp").as_deref(),
            Some("/home/admin/x.lzp")
        );
        assert_eq!(normalise("/transient/x/").as_deref(), Some("/transient/x"));
        for bad in [
            "",
            "/",
            "x.lzp",
            "/..",
            "/a/../..",
            "/transient/x\n",
            "/transient/\0",
        ] {
            assert_eq!(normalise(bad), None, "{bad:?}");
        }
    }

    const USER: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 0,
        system: false,
    };
    const OTHER: Caller = Caller {
        uid: 1001,
        session: 4,
        label_id: 0,
        system: false,
    };
    const USER_HOME: Option<&str> = Some("/home/user");
    const OTHER_HOME: Option<&str> = Some("/home/other");

    /// The matrix of issue #508 section 5.
    #[test]
    fn the_install_source_rule() {
        let allowed = |caller: &Caller, home, path| source_allowed(caller, home, path).is_ok();
        assert!(allowed(&USER, USER_HOME, "/transient/x.lzp"));
        assert!(allowed(&OTHER, OTHER_HOME, "/transient/x.lzp"));
        assert!(allowed(&USER, USER_HOME, "/home/user/x.lzp"));
        assert!(!allowed(&OTHER, OTHER_HOME, "/home/user/x.lzp"));
        assert!(!allowed(&USER, USER_HOME, "/home/user/../admin/x.lzp"));
        // The image's samples are public data, for every user (issue #623).
        assert!(allowed(
            &USER,
            USER_HOME,
            "/system/share/samples/pkgdemo.lzp"
        ));
        assert!(allowed(&ROOT, None, "/system/share/samples/pkgdemo.lzp"));
        assert!(!allowed(&USER, USER_HOME, "/system/etc/x.lzp"));
        assert!(!allowed(&ROOT_SESSION, None, "/home/other/x.lzp"));
        let refusal = source_allowed(&USER, USER_HOME, "/system/etc/x.lzp");
        assert_eq!(refusal.unwrap_err(), SOURCE_RULE);
    }

    #[test]
    fn the_normalised_path_is_what_is_read() {
        assert_eq!(
            source_allowed(&USER, USER_HOME, "/transient//a/../x.lzp").unwrap(),
            "/transient/x.lzp"
        );
        // A climb out of `/transient` is judged where it lands.
        assert!(source_allowed(&USER, USER_HOME, "/transient/../conf/store").is_err());
        assert!(source_allowed(&USER, USER_HOME, "/home/user/../../conf/store").is_err());
    }

    #[test]
    fn root_may_read_any_absolute_path() {
        assert!(source_allowed(&ROOT, None, "/home/other/secret.lzp").is_ok());
        assert!(source_allowed(&ROOT, None, "/..").is_err());
        assert!(source_allowed(&ROOT, None, "relative.lzp").is_err());
    }

    #[test]
    fn a_user_is_confined_to_readable_by_design_places() {
        for bad in [
            "/home/other/pkgdemo.lzp",
            "/home/username/pkgdemo.lzp",
            "/home/user",
            "/conf/store",
            "/apps/org.lazy.x.y/1.0.0-00000000/manifest.toml",
            "/tmp/pkgdemo.lzp",
            "/data/home/user/x.lzp",
            "/transientx/a",
            "/transient",
            "/PKGDEMO.LZP",
        ] {
            assert!(source_allowed(&USER, USER_HOME, bad).is_err(), "{bad}");
        }
        // Without a known home, only the shared place.
        assert!(source_allowed(&USER, None, "/home/user/x.lzp").is_err());
        assert!(source_allowed(&USER, None, "/transient/x.lzp").is_ok());
        // A malformed home grants nothing.
        assert!(source_allowed(&USER, Some("/"), "/anything/x").is_err());
        assert!(source_allowed(&USER, Some("/home/../"), "/home/x").is_err());
    }
}

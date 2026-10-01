//! Who may ask `pkgd` for what, and which package files it will read for them.
//!
//! `pkgd` runs as root so it can write `/data/apps` and load kernel policy.
//! That makes it a confused deputy for the paths it is handed: reading a file
//! *as root* on a user's say-so would let that user install (and so copy out,
//! into world-readable `/data/apps`) a package they could not read themselves.
//! There is no "open as uid" call, so [`source_allowed`] confines an
//! unprivileged caller to locations that are readable by design: the boot
//! volume root, the shared `/tmp`, and the caller's own home directory. Root
//! may name any absolute path.

use alloc::string::String;

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
}

/// Whether `caller` may install or remove applications: root, or the owner of a
/// login session. A sandboxed application never may, however it is labelled
/// (the kernel's default-deny already stops it; this is the second lock).
pub fn may_manage(caller: &Caller) -> Result<(), &'static str> {
    if caller.label_id != 0 {
        return Err("applications may not install or remove other applications");
    }
    if caller.uid == 0 || caller.session != 0 {
        Ok(())
    } else {
        Err("only a logged-in user or the administrator may install or remove applications")
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

/// Whether `path` lies at or under `root` (a directory path without a trailing
/// slash), component-wise: `/tmp/x` is under `/tmp`, `/tmpx` is not.
fn under(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// Whether `pkgd` will read the package at `path` for `caller`. `home` is the
/// caller's home directory from the account database, when known.
pub fn source_allowed(caller: &Caller, home: Option<&str>, path: &str) -> Result<(), String> {
    if !well_formed(path) {
        return Err(String::from("that is not a valid absolute file path"));
    }
    if caller.uid == 0 {
        return Ok(());
    }
    let boot_volume_root = path[1..].find('/').is_none();
    let own_home = home.is_some_and(|home| well_formed(home) && under(path, home));
    if boot_volume_root || under(path, "/tmp") || own_home {
        Ok(())
    } else {
        Err(String::from(
            "packages can only be installed from the boot volume, /tmp or your own home folder",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: Caller = Caller {
        uid: 0,
        session: 0,
        label_id: 0,
    };
    const ALICE: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 0,
    };
    const DAEMON: Caller = Caller {
        uid: 901,
        session: 0,
        label_id: 0,
    };
    const SANDBOXED: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 7,
    };

    #[test]
    fn root_and_session_owners_manage_apps() {
        assert!(may_manage(&ROOT).is_ok());
        assert!(may_manage(&ALICE).is_ok());
        assert!(may_manage(&DAEMON).is_err());
        let sandboxed_root = Caller {
            uid: 0,
            ..SANDBOXED
        };
        assert!(may_manage(&SANDBOXED).is_err());
        assert!(may_manage(&sandboxed_root).is_err());
    }

    #[test]
    fn a_sandboxed_app_may_not_even_inspect() {
        assert!(may_inspect(&ALICE).is_ok());
        assert!(may_inspect(&DAEMON).is_ok());
        assert!(may_inspect(&SANDBOXED).is_err());
    }

    #[test]
    fn well_formed_paths() {
        for good in ["/PKGDEMO.LZP", "/tmp/a.lzp", "/data/home/alice/a b.lzp"] {
            assert!(well_formed(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "relative.lzp",
            "/tmp/../etc/x",
            "/tmp/./x",
            "/tmp//x",
            "/tmp/x\0",
            "/tmp/x\n",
            &format!("/{}", "a".repeat(MAX_PATH)),
        ] {
            assert!(!well_formed(bad), "{bad:?}");
        }
    }

    #[test]
    fn root_may_read_any_well_formed_path() {
        assert!(source_allowed(&ROOT, None, "/data/home/bob/secret.lzp").is_ok());
        assert!(source_allowed(&ROOT, None, "/tmp/../x").is_err());
    }

    #[test]
    fn a_user_is_confined_to_readable_by_design_places() {
        let home = Some("/data/home/alice");
        for good in [
            "/PKGDEMO.LZP",
            "/tmp/pkgdemo.lzp",
            "/data/home/alice/Downloads/pkgdemo.lzp",
        ] {
            assert!(source_allowed(&ALICE, home, good).is_ok(), "{good}");
        }
        for bad in [
            "/data/home/bob/pkgdemo.lzp",
            "/data/home/alicia/pkgdemo.lzp",
            "/data/home/alice",
            "/data/confd/store",
            "/data/apps/org.lazy.x.y/1.0.0-00000000/manifest.toml",
            "/etc/passwd",
            "/tmpx/a",
            "/tmp/",
        ] {
            assert!(source_allowed(&ALICE, home, bad).is_err(), "{bad}");
        }
        // Without a known home, only the shared places.
        assert!(source_allowed(&ALICE, None, "/data/home/alice/x.lzp").is_err());
        // A malformed home grants nothing.
        assert!(source_allowed(&ALICE, Some("/"), "/anything/x").is_err());
        assert!(source_allowed(&ALICE, Some("/data/../"), "/data/x").is_err());
    }
}

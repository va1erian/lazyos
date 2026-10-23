//! The file gate: every path operation of a confined label, through the Linux
//! ABI, the native filesystem calls and the VFS itself.

use super::*;

/// Every file here is readable by the user without the label (but the font
/// and the program files, which are world-readable anyway): `(path, may
/// open for reading, may open for writing)` under [`RULES`].
const MATRIX: &[(&str, bool, bool)] = &[
    (FONT, true, false),
    ("/system/share/fonts", true, false),
    ("/system/bin/busybox", false, false),
    ("/system/bin/top", false, false),
    ("/conf/confd.db", false, false),
    ("/home/user/.ssh/id_rsa", false, false),
    ("/home/user/notes.txt", false, false),
    ("/home/user/Documents/cv.txt", false, false),
    (COOKIES, true, true),
    ("/home/user/.apps/org.lazy.mail/mail.db", false, false),
    ("/home/admin/secret", false, false),
    ("/transient/net/resolv.conf", false, false),
    ("/etc/resolv.conf", true, false),
    ("/etc/hosts", false, false),
    ("/etc/passwd", false, false),
];

/// What a confined label opens, for reading and for writing, against the
/// rules, and that a refusal is `EPERM` whatever the mode bits say.
pub fn files_matrix() -> Result<(), String> {
    with_world(|| {
        // Control: without the label the user opens every file but the
        // admin's (and cannot write what is not theirs).
        as_plain_user();
        for path in [
            "/home/user/.ssh/id_rsa",
            "/home/user/notes.txt",
            "/system/bin/busybox",
        ] {
            check!(opens(path, O_RDONLY), "the plain user cannot read {path}");
        }
        check!(
            !opens("/home/admin/secret", O_RDONLY),
            "the plain user read the admin's file"
        );

        confine_as_user(RULES, &[])?;
        for &(path, read, write) in MATRIX {
            let got = openat(path, O_RDONLY);
            check!(
                (got >= 0) == read,
                "open {path} for reading gave {got}, want {}",
                if read { "a descriptor" } else { "EPERM" }
            );
            close(got);
            if !read {
                check!(
                    got == EPERM,
                    "open {path} was refused with {got}, not EPERM"
                );
            }
            let got = openat(path, O_WRONLY);
            check!(
                (got >= 0) == write,
                "open {path} for writing gave {got}, want {}",
                if write { "a descriptor" } else { "a refusal" }
            );
            close(got);
        }
        // A refusal for access the rules do not grant is the policy's, and
        // it comes before the mode bits: the font is `0644` root, which the
        // user may not write either way, and still answers `EPERM`.
        check!(openat(FONT, O_RDWR) == EPERM, "O_RDWR on a read-only grant");
        Ok(())
    })
}

/// Creating, removing, renaming and changing: only inside a write grant,
/// and a refused call changes nothing.
pub fn creation_and_change() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        let new = format!("{PROFILE}/new.db");
        let fd = openat(&new, O_WRONLY | O_CREAT);
        check!(fd >= 0, "create in the profile directory gave {fd}");
        close(fd);
        for path in [
            "/home/user/.apps/evil.db",
            "/home/user/.ssh/authorized_keys",
            "/home/user/dropped",
            "/system/share/fonts/evil.ttf",
            "/system/bin/evil",
            "/transient/x",
            "/etc/evil",
        ] {
            let got = openat(path, O_WRONLY | O_CREAT);
            check!(got == EPERM, "create {path} gave {got}");
        }

        check!(
            mkdir(&format!("{PROFILE}/cache")) == 0,
            "mkdir in the profile directory"
        );
        // A name that is there is EEXIST for whoever may look at it, in a directory they may not
        // write (a program making every component of a path walks through them), and EPERM for
        // whoever may not: the gate does not tell existence.
        for path in [
            "/home",
            "/home/user",
            "/home/user/.apps",
            PROFILE,
            "/system/share",
        ] {
            check!(
                mkdir(path) == EEXIST,
                "mkdir of the visible {path}: {}",
                mkdir(path)
            );
        }
        for path in [
            "/home/user/.ssh",
            "/home/user/Documents",
            "/system/bin",
            "/conf",
        ] {
            check!(
                mkdir(path) == EPERM,
                "mkdir of the hidden {path}: {}",
                mkdir(path)
            );
        }
        for path in ["/home/user/evil", "/home/user/.apps/x", "/system/share/x"] {
            check!(mkdir(path) == EPERM, "mkdir {path}");
        }
        check!(
            rmdir(&format!("{PROFILE}/cache")) == 0,
            "rmdir in the profile"
        );
        check!(rmdir("/home/user/Documents") == EPERM, "rmdir outside");
        check!(unlink(&new) == 0, "unlink in the profile directory");
        for path in [
            "/home/user/notes.txt",
            "/home/user/.apps/org.lazy.mail/mail.db",
            FONT,
        ] {
            check!(unlink(path) == EPERM, "unlink {path}");
        }

        let bak = format!("{COOKIES}.bak");
        check!(rename(COOKIES, &bak) == 0, "rename inside the profile");
        check!(rename(&bak, COOKIES) == 0, "rename back");
        check!(
            rename(COOKIES, "/home/user/stolen") == EPERM,
            "rename out of the profile"
        );
        check!(
            rename("/home/user/notes.txt", &format!("{PROFILE}/notes.txt")) == EPERM,
            "rename into the profile from outside"
        );
        check!(
            rename(
                "/home/user/Documents/cv.txt",
                "/home/user/Documents/cv2.txt"
            ) == EPERM,
            "rename outside"
        );

        check!(chmod(COOKIES, 0o600) == 0, "chmod inside the profile");
        check!(
            chmod("/home/user/notes.txt", 0o777) == EPERM,
            "chmod outside"
        );
        check!(chmod(FONT, 0o777) == EPERM, "chmod on a read-only grant");
        check!(truncate(COOKIES, 0) == 0, "truncate inside the profile");
        check!(
            truncate("/home/user/notes.txt", 0) == EPERM,
            "truncate outside"
        );

        // Nothing a refusal named changed (read as the kernel).
        let root = Id::ROOT;
        let notes = crate::fs::abi_read(root, "/home/user/notes.txt").map_err(fs_error)?;
        check!(notes == b"notes", "notes.txt changed to {notes:?}");
        let meta = crate::fs::abi_stat(root, "/home/user/notes.txt").map_err(fs_error)?;
        check!(meta.mode & 0o777 == 0o644, "notes.txt mode {:o}", meta.mode);
        for path in [
            "/home/user/Documents/cv.txt",
            "/home/user/.apps/org.lazy.mail/mail.db",
            FONT,
        ] {
            check!(
                crate::fs::abi_stat(root, path).is_ok(),
                "{path} is gone after a refused unlink or rename"
            );
        }
        for path in ["/home/user/stolen", "/home/user/evil", "/home/user/dropped"] {
            check!(
                crate::fs::abi_stat(root, path).is_err(),
                "{path} was created by a refused call"
            );
        }
        Ok(())
    })
}

/// A confined task may look at the ancestors of its grants (a path walk,
/// `realpath`, `access(F_OK)`), not read them, and sees no sibling.
pub fn ancestors_visible() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        for path in [
            "/",
            "/home",
            "/home/user",
            "/home/user/.apps",
            PROFILE,
            COOKIES,
            "/system",
            "/system/share",
            "/system/share/fonts",
            "/etc",
            "/etc/ssl",
            "/etc/ssl/certs",
        ] {
            check!(stat(path) == 0, "stat {path} gave {}", stat(path));
            check!(access(path, 0) == 0, "access(F_OK) {path}");
        }
        for path in [
            "/home/user/.ssh",
            "/home/user/Documents",
            "/home/user/.apps/org.lazy.mail",
            "/system/bin",
            "/system/etc",
            "/conf",
            "/transient",
            "/etc/hosts",
            "/etc/ssl/private",
        ] {
            check!(stat(path) == EPERM, "stat {path} gave {}", stat(path));
        }
        // Looking is not reading or listing.
        for path in [
            "/home",
            "/home/user",
            "/home/user/.apps",
            "/system",
            "/etc",
            "/",
        ] {
            check!(
                !opens(path, O_RDONLY | O_DIRECTORY),
                "listing the ancestor {path} was allowed"
            );
            check!(access(path, 4) == EPERM, "access(R_OK) {path}");
        }
        for path in [PROFILE, "/system/share/fonts"] {
            check!(
                opens(path, O_RDONLY | O_DIRECTORY),
                "listing the granted {path} was refused"
            );
        }
        Ok(())
    })
}

/// `..`, `//`, a relative path from a granted working directory, a directory
/// descriptor and an absolute path through one: all fold to the one path the
/// rules are asked about.
pub fn dot_dot_and_dirfds() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        for path in [
            "/../../home/user/.ssh/id_rsa",
            "/system/share/fonts/../../../home/user/.ssh/id_rsa",
            "/home/user/.apps/os.lazy.lazyweb/../../.ssh/id_rsa",
            "/home/user/.apps/os.lazy.lazyweb/./../.././.ssh/id_rsa",
            "//home//user//.ssh//id_rsa",
        ] {
            check!(openat(path, O_RDONLY) == EPERM, "open {path}");
        }
        for path in [
            "/system/share/fonts/../fonts/liberation/LiberationSans-Regular.ttf",
            "/home/user/.apps/org.lazy.mail/../os.lazy.lazyweb/cookie.jar.db",
            "//system//share//fonts//liberation//LiberationSans-Regular.ttf",
        ] {
            check!(opens(path, O_RDONLY), "{path} names a granted file");
        }

        // A relative path starts from the working directory, which a confined
        // task can only move into a granted directory.
        check!(chdir(PROFILE) == 0, "chdir into the profile");
        check!(opens("cookie.jar.db", O_RDONLY), "relative open of a grant");
        for path in [
            "../../.ssh/id_rsa",
            "../org.lazy.mail/mail.db",
            "../../../../etc/passwd",
        ] {
            check!(openat(path, O_RDONLY) == EPERM, "relative open {path}");
        }
        check!(
            chdir("/home/user/.ssh") == EPERM,
            "chdir into a private directory"
        );
        check!(chdir("/home/user") == EPERM, "chdir into an ancestor");
        check!(chdir("..") == EPERM, "chdir to the parent of a grant");
        task::set_cwd("/");

        // A directory descriptor is its recorded path: `openat` from it
        // cannot leave the grant, and an absolute path ignores it.
        let dirfd = openat(PROFILE, O_RDONLY | O_DIRECTORY);
        check!(dirfd >= 0, "open the profile directory: {dirfd}");
        let inside = openat_at(dirfd, "cookie.jar.db", O_RDONLY);
        check!(inside >= 0, "openat inside the directory: {inside}");
        close(inside);
        for path in [
            "../../.ssh/id_rsa",
            "../../../../home/user/.ssh/id_rsa",
            "/home/user/.ssh/id_rsa",
            "/home/admin/secret",
        ] {
            let got = openat_at(dirfd, path, O_RDONLY);
            check!(got == EPERM, "openat(dirfd, {path}) gave {got}");
        }
        close(dirfd);
        Ok(())
    })
}

/// The entries the kernel fabricates are named by paths too: the data
/// devices and the machine's `/proc` come without a rule, the terminal, pty
/// and account files do not.
pub fn fabricated_entries() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        for path in ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"] {
            check!(opens(path, O_RDONLY), "{path} for reading");
        }
        for path in ["/dev/null", "/dev/full", "/dev/zero"] {
            check!(opens(path, O_WRONLY), "{path} for writing");
        }
        check!(!opens("/dev/urandom", O_WRONLY), "/dev/urandom for writing");
        for path in [
            "/dev/tty",
            "/dev/console",
            "/dev/tty0",
            "/dev/ptmx",
            "/dev/pts/ptmx",
            "/dev/pts/0",
            "/dev/mem",
            "/dev/kmsg",
        ] {
            check!(openat(path, O_RDWR) == EPERM, "open {path}");
        }
        for path in [
            "/proc/self/status",
            "/proc/self/stat",
            "/proc/meminfo",
            "/proc/cpuinfo",
            "/proc/self/mountinfo",
        ] {
            check!(opens(path, O_RDONLY), "{path} for reading");
            check!(!opens(path, O_WRONLY), "{path} for writing");
        }
        for path in [
            "/etc/passwd",
            "/etc/group",
            "/etc/os-release",
            "/etc/shells",
        ] {
            check!(openat(path, O_RDONLY) == EPERM, "open {path}");
        }
        // The same entries for the plain user, so the refusals above are the
        // label's.
        as_plain_user();
        for path in ["/etc/os-release", "/dev/null", "/proc/self/status"] {
            check!(opens(path, O_RDONLY), "the plain user cannot open {path}");
        }
        Ok(())
    })
}

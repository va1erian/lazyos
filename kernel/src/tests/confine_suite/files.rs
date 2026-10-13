//! The file gate: every path operation of a confined label, through the Linux
//! ABI, the native filesystem calls and the VFS itself.

use super::*;
use crate::fs::vfs::AttrRequest;

const FONT: &str = "/system/share/fonts/liberation/LiberationSans-Regular.ttf";
const COOKIES: &str = "/home/user/.apps/os.lazy.webkit/cookie.jar.db";
const PROFILE: &str = "/home/user/.apps/os.lazy.webkit";

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
            "/home/user/.apps/os.lazy.webkit/../../.ssh/id_rsa",
            "/home/user/.apps/os.lazy.webkit/./../.././.ssh/id_rsa",
            "//home//user//.ssh//id_rsa",
        ] {
            check!(openat(path, O_RDONLY) == EPERM, "open {path}");
        }
        for path in [
            "/system/share/fonts/../fonts/liberation/LiberationSans-Regular.ttf",
            "/home/user/.apps/org.lazy.mail/../os.lazy.webkit/cookie.jar.db",
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

/// A confined label starts nothing with `execve`: not a program it could
/// read, not a native program, not a BusyBox applet, not a granted file.
pub fn execve_refused() -> Result<(), String> {
    with_world(|| {
        // The program files of this suite are not ELF images, so a gate that
        // let one through would fail with `ENOEXEC` here instead of replacing
        // the test harness's own image.
        as_plain_user();
        let plain = execve("/system/bin/busybox");
        check!(plain == ENOEXEC, "the plain user's execve gave {plain}");

        // Even with a read rule for the programs themselves: a confined label
        // starts nothing.
        let mut rules: Vec<&str> = RULES.to_vec();
        rules.push("read:/system/bin");
        confine_as_user(&rules, &[])?;
        for path in [
            "/system/bin/busybox",
            "/system/bin/top",
            "/bin/sh",
            "sh",
            "/usr/bin/env",
            FONT,
            COOKIES,
        ] {
            let got = execve(path);
            check!(got == EPERM, "execve {path} gave {got}");
        }
        Ok(())
    })
}

/// The native syscalls reach the same VFS: a confined Linux process that
/// issues them with `int 0x80` is held to the same rules.
pub fn native_calls() -> Result<(), String> {
    with_world(|| {
        confine_as_user(RULES, &[])?;
        let none = u64::MAX;
        let dispatch = |nr, a, b, c3| process::dispatch_for_test(nr, a, b, c3);
        let mut buf = [0u8; 256];
        let p = |text: &Vec<u8>| text.as_ptr() as u64;

        // 3 read_file
        let (font, key) = (c(FONT), c("/home/user/.ssh/id_rsa"));
        let got = dispatch(3, p(&font), buf.as_mut_ptr() as u64, 64);
        check!(got == 10, "read_file of a grant gave {got}");
        let got = dispatch(3, p(&key), buf.as_mut_ptr() as u64, 64);
        check!(got == none, "read_file of a private file gave {got:#x}");

        let eperm = (-1i64) as u64;
        // 15 stat, 16 readdir, 18 mkdir, 19 unlink, 32 chmod
        let mut out = [0u64; 2];
        check!(
            dispatch(15, p(&c(COOKIES)), out.as_mut_ptr() as u64, 0) == 0,
            "stat of a grant"
        );
        check!(
            dispatch(15, p(&key), out.as_mut_ptr() as u64, 0) == eperm,
            "stat of a private file"
        );
        check!(
            dispatch(16, p(&c(PROFILE)), buf.as_mut_ptr() as u64, 256) < 256,
            "readdir of a grant"
        );
        check!(
            dispatch(16, p(&c("/home/user")), buf.as_mut_ptr() as u64, 256) == eperm,
            "readdir of an ancestor"
        );
        check!(
            dispatch(18, p(&c(&format!("{PROFILE}/n"))), 0, 0) == 0,
            "mkdir in a grant"
        );
        check!(
            dispatch(18, p(&c("/home/user/n")), 0, 0) == eperm,
            "mkdir outside"
        );
        check!(
            dispatch(19, p(&c(&format!("{PROFILE}/n"))), 0, 0) == 0,
            "unlink in a grant"
        );
        check!(
            dispatch(19, p(&c("/home/user/notes.txt")), 0, 0) == eperm,
            "unlink outside"
        );
        check!(
            dispatch(20, p(&c("/home/user/notes.txt")), p(&c("/home/user/x")), 0) == eperm,
            "rename outside"
        );
        check!(
            dispatch(32, p(&c("/home/user/notes.txt")), 0o777, 0) == eperm,
            "chmod outside"
        );
        let data = b"hello";
        check!(
            dispatch(
                17,
                p(&c(&format!("{PROFILE}/w.txt"))),
                data.as_ptr() as u64,
                5
            ) == 5,
            "write_file in a grant"
        );
        check!(
            dispatch(17, p(&c("/home/user/w.txt")), data.as_ptr() as u64, 5) == eperm,
            "write_file outside"
        );
        check!(
            dispatch(28, p(&c("/home/user/notes.txt")), data.as_ptr() as u64, 5) == eperm,
            "append_file outside"
        );
        let request = [buf.as_mut_ptr() as u64, 16, 0];
        check!(
            dispatch(30, p(&key), request.as_ptr() as u64, 0) == eperm,
            "read_at of a private file"
        );
        // The private file is as it was.
        let kept = crate::fs::vfs_read(Id::ROOT, "/home/user/.ssh/id_rsa").map_err(fs_error)?;
        check!(kept == b"PRIVATE KEY", "the key changed");
        Ok(())
    })
}

/// Every entry point of the VFS that takes an identity refuses a confined
/// label outside its grants, with `NotPermitted`, and none refuses it inside:
/// a new `Vfs` method that skips the gate shows up here.
pub fn every_vfs_entry_point() -> Result<(), String> {
    with_world(|| {
        let label = confine_as_user(RULES, &[])?;
        let confined = Id::with_label(1000, 100, label);
        let plain = Id::new(1000, 100);
        let mut vfs = table()?;
        let outside = "/home/user/notes.txt";
        let other = "/home/user/n2.txt";
        let dir = "/home/user/Documents";
        let refused = |what: &str, got: Result<(), FsError>| -> Result<(), String> {
            check!(
                got == Err(FsError::NotPermitted),
                "{what} was not refused by the gate: {got:?}"
            );
            Ok(())
        };
        refused("stat", vfs.stat(confined, outside).map(|_| ()))?;
        refused("check", vfs.check(confined, outside, 4).map(|_| ()))?;
        refused("read_file", vfs.read_file(confined, outside).map(|_| ()))?;
        let mut buf = [0u8; 8];
        refused("read", vfs.read(confined, outside, 0, &mut buf).map(|_| ()))?;
        refused("write", vfs.write(confined, outside, 0, b"x").map(|_| ()))?;
        refused("truncate", vfs.truncate(confined, outside, 0))?;
        refused("create", vfs.create(confined, other, 0o644).map(|_| ()))?;
        refused(
            "mkdir",
            vfs.mkdir(confined, "/home/user/d2", 0o755).map(|_| ()),
        )?;
        refused("unlink", vfs.unlink(confined, outside))?;
        refused("rmdir", vfs.rmdir(confined, dir))?;
        refused("rename", vfs.rename(confined, outside, other))?;
        refused("readdir", vfs.readdir(confined, dir).map(|_| ()))?;
        refused(
            "setattr",
            vfs.setattr(confined, outside, AttrRequest::Mode(0o600))
                .map(|_| ()),
        )?;
        refused(
            "setattr_open",
            vfs.setattr_open(confined, outside, AttrRequest::Mode(0o600))
                .map(|_| ()),
        )?;
        refused("flush", vfs.flush(confined, outside))?;
        refused("statfs", vfs.statfs(confined, outside).map(|_| ()))?;
        refused("open_node", vfs.open_node(confined, outside, 4).map(|_| ()))?;

        // The same calls as the plain user are not refused by the gate.
        check!(vfs.stat(plain, outside).is_ok(), "plain stat");
        check!(vfs.read_file(plain, outside).is_ok(), "plain read_file");
        check!(vfs.write(plain, outside, 0, b"x").is_ok(), "plain write");
        check!(vfs.readdir(plain, dir).is_ok(), "plain readdir");

        // And the grants work through the same calls.
        check!(
            vfs.read_file(confined, FONT).is_ok(),
            "confined read of a grant"
        );
        check!(
            vfs.stat(confined, COOKIES).is_ok(),
            "confined stat of a grant"
        );
        check!(
            vfs.write(confined, COOKIES, 0, b"x").is_ok(),
            "confined write of a grant"
        );
        check!(
            vfs.readdir(confined, PROFILE).is_ok(),
            "confined readdir of a grant"
        );
        check!(
            vfs.create(confined, &format!("{PROFILE}/n"), 0o644).is_ok(),
            "confined create in a grant"
        );
        // An identity that carries a label that is not confined is untouched.
        let free = labels::intern("app:os.lazy.free").map_err(|e| format!("{e:?}"))?;
        check!(
            vfs.read_file(Id::with_label(1000, 100, free), outside)
                .is_ok(),
            "an unconfined label was gated"
        );
        Ok(())
    })
}

/// A label with no confinement rules, an unlabelled task and the kernel are
/// not gated.
pub fn unconfined_untouched() -> Result<(), String> {
    with_world(|| {
        let label = labels::intern("app:os.lazy.free").map_err(|e| format!("{e:?}"))?;
        // Rules, but no file ones: an ordinary installed app.
        load_ids(label, &[0x1234_5678_9abc_def0])?;
        credentials::set(
            task::KERNEL_TASK,
            Cred::new(USER_CRED.0, USER_CRED.1, 0, label, 0),
        );
        check!(!confine::confined(label), "an ordinary label is confined");
        for path in ["/home/user/.ssh/id_rsa", "/system/bin/busybox", "/dev/null"] {
            check!(opens(path, O_RDONLY), "{path} refused to an ordinary app");
        }
        check!(
            unlink("/home/user/notes.txt") == 0,
            "an ordinary app could not remove its user's file"
        );
        as_plain_user();
        check!(opens("/home/user/.ssh/id_rsa", O_RDONLY), "unlabelled read");
        credentials::set(task::KERNEL_TASK, Cred::ROOT);
        check!(opens("/conf/confd.db", O_RDONLY), "root read");
        Ok(())
    })
}

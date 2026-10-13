//! The calls that are not a path through the Linux file syscalls: `execve`, the native
//! filesystem calls, every `Vfs` entry point, and the labels that are not confined.

use super::*;
use crate::fs::vfs::AttrRequest;

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
        // POSIX: an existing name is EEXIST even where the directory above is not the caller's.
        check!(
            mkdir("/system/share") == EEXIST,
            "mkdir of an existing directory"
        );
        check!(
            mkdir("/system/newdir") == EACCES,
            "mkdir in a directory that is not the user's"
        );
        credentials::set(task::KERNEL_TASK, Cred::ROOT);
        check!(opens("/conf/confd.db", O_RDONLY), "root read");
        Ok(())
    })
}

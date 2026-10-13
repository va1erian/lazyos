//! Confined labels (issue #832, `ipc::confine`): what a sandboxed process may
//! open, create, change, execute, connect to, start and signal, judged
//! through the real Linux ABI and native syscall entry points and the VFS.
//!
//! The harness task is given the credentials of a user (uid 1000) under a
//! label whose rules are exactly the rule ids `filepolicy` compiles from a
//! manifest's `files`, so a refusal here is the label's and never the mode
//! bits': every private file below is owned by the user, who could read it
//! without the label.
//!
//! * [`files`]: the path matrix through `openat`, `stat`, `mkdir`, `unlink`,
//!   `rename`, `chmod`, `chdir`, directory descriptors, `..` and the
//!   fabricated `/dev`, `/proc` and `/etc`;
//! * [`entry`]: `execve`, the native filesystem calls, every `Vfs` entry
//!   point and the labels that are not confined;
//! * [`procs`]: sockets, process creation, signals and the calls that must
//!   stay unimplemented (`ptrace`, `process_vm_readv`, `mount`, ...);
//! * [`tty`]: a confined task reads end of file from a terminal;
//! * [`policy`]: rule reloads, revocation, the wildcard rule, the audit
//!   trail, and a soak of many allowed and refused calls.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{FsError, Id, MountFlags, Vfs};
use crate::ipc::acl::{self, Rule, ANY_METHOD};
use crate::ipc::credentials::{self, Cred};
use crate::ipc::{audit, confine, labels};
use alloc::sync::Arc;

mod entry;
mod files;
mod policy;
mod procs;
mod tty;

/// The user the harness task runs as, and who owns the "private" files.
const USER: Id = Id::new(1000, 100);
const USER_CRED: (u32, u32) = (1000, 100);
/// Another user.
const ADMIN: Id = Id::new(1001, 100);

/// `-EPERM` as a syscall result.
const EPERM: i64 = -1;
const ENOEXEC: i64 = -8;
const EACCES: i64 = -13;
const EEXIST: i64 = -17;
const ENOSYS: i64 = -38;
const AT_FDCWD: u64 = (-100i64) as u64;
const O_RDONLY: u64 = 0;
const O_WRONLY: u64 = 0o1;
const O_RDWR: u64 = 0o2;
const O_CREAT: u64 = 0o100;
const O_DIRECTORY: u64 = 0o200000;

/// Paths of the world: a granted font, the profile directory and a file in it, and a private file.
pub(super) const FONT: &str = "/system/share/fonts/liberation/LiberationSans-Regular.ttf";
pub(super) const PROFILE: &str = "/home/user/.apps/os.lazy.webkit";
pub(super) const COOKIES: &str = "/home/user/.apps/os.lazy.webkit/cookie.jar.db";
pub(super) const NOTES: &str = "/home/user/notes.txt";

/// What the sandbox of these tests is granted, in the manifest's words: the
/// fonts to read, its own profile directory to write, the CA bundle and the
/// resolver to read.
pub(super) const RULES: &[&str] = &[
    "read:/system/share/fonts",
    "write:$HOME/.apps/os.lazy.webkit",
    "read:/etc/ssl/certs/ca-certificates.crt",
    "read:/etc/resolv.conf",
];

/// The label the tests confine.
pub(super) const LABEL: &str = "app:os.lazy.sandbox";

pub(super) fn fs_error(error: FsError) -> String {
    String::from(error.message())
}

fn dir(vfs: &mut Vfs, id: Id, path: &str, mode: u16) -> Result<(), String> {
    vfs.mkdir(id, path, mode).map(|_| ()).map_err(fs_error)
}

fn file(vfs: &mut Vfs, id: Id, path: &str, mode: u16, data: &[u8]) -> Result<(), String> {
    vfs.create(id, path, mode).map_err(fs_error)?;
    vfs.write(id, path, 0, data).map_err(fs_error)?;
    Ok(())
}

/// One mount table holding the files of this suite, all on a ramfs.
fn table() -> Result<Vfs, String> {
    let mut vfs = Vfs::new();
    vfs.mount("/", Arc::new(RamFs::new()), MountFlags::default())
        .map_err(fs_error)?;
    vfs.set_umask(0);
    let root = Id::ROOT;
    // The program files are text, so that an `execve` that gets past the gate
    // fails with `ENOEXEC` instead of replacing the test harness; `helper`
    // is a real (minimal) program for the spawn tests.
    let elf = service_suite::minimal_elf();
    for path in [
        "/system",
        "/system/share",
        "/system/share/fonts",
        "/system/share/fonts/liberation",
        "/system/bin",
        "/conf",
        "/transient",
        "/transient/net",
    ] {
        dir(&mut vfs, root, path, 0o755)?;
    }
    // Writable by everyone, so that each user can make their own home below
    // it (the image build creates and chowns them; this suite has no chown).
    dir(&mut vfs, root, "/home", 0o777)?;
    file(
        &mut vfs,
        root,
        "/system/share/fonts/liberation/LiberationSans-Regular.ttf",
        0o644,
        b"font bytes",
    )?;
    file(
        &mut vfs,
        root,
        "/system/bin/busybox",
        0o755,
        b"not a program",
    )?;
    file(&mut vfs, root, "/system/bin/top", 0o755, b"not a program")?;
    file(&mut vfs, root, "/system/bin/helper", 0o755, &elf)?;
    file(&mut vfs, root, "/conf/confd.db", 0o600, b"settings")?;
    file(
        &mut vfs,
        root,
        "/transient/net/resolv.conf",
        0o644,
        b"nameserver 10.0.2.3\n",
    )?;
    // The user's home: private files the user could read without the label.
    for path in [
        "/home/user",
        "/home/user/.ssh",
        "/home/user/.apps",
        "/home/user/.apps/os.lazy.webkit",
        "/home/user/.apps/org.lazy.mail",
        "/home/user/Documents",
    ] {
        dir(&mut vfs, USER, path, 0o755)?;
    }
    file(
        &mut vfs,
        USER,
        "/home/user/.ssh/id_rsa",
        0o600,
        b"PRIVATE KEY",
    )?;
    file(&mut vfs, USER, "/home/user/notes.txt", 0o644, b"notes")?;
    file(&mut vfs, USER, "/home/user/Documents/cv.txt", 0o644, b"cv")?;
    file(
        &mut vfs,
        USER,
        "/home/user/.apps/os.lazy.webkit/cookie.jar.db",
        0o644,
        b"cookies",
    )?;
    file(
        &mut vfs,
        USER,
        "/home/user/.apps/org.lazy.mail/mail.db",
        0o600,
        b"mail",
    )?;
    dir(&mut vfs, ADMIN, "/home/admin", 0o700)?;
    file(&mut vfs, ADMIN, "/home/admin/secret", 0o600, b"admin")?;
    Ok(vfs)
}

/// Run `body` as the kernel task with this suite's tables installed (the
/// native and the Linux ABI one are separate but identical), no labels and
/// root credentials; put everything back afterwards.
pub(super) fn with_world(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    acl::reset_labels_for_tests();
    labels::reset_for_tests();
    confine::reset_for_tests();
    audit::reset();
    let native = crate::fs::install_native_for_test(table()?);
    let abi = crate::fs::install_abi_for_test(table()?);
    task::set_cwd("/");
    let result = body();
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
    task::set_cwd("/");
    crate::fs::restore_abi_for_test(abi);
    crate::fs::restore_native_for_test(native);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    acl::reset_labels_for_tests();
    labels::reset_for_tests();
    confine::reset_for_tests();
    task::harness::reset();
    result
}

/// Load `ids` as allow rules of `label` (any method), replacing its rules.
pub(super) fn load_ids(label: u32, ids: &[u64]) -> Result<(), String> {
    let rules: Vec<Rule> = ids
        .iter()
        .map(|&interface_id| Rule {
            actor: label,
            interface_id,
            method: ANY_METHOD,
            allow: true,
        })
        .collect();
    acl::load_label(label, &rules).map_err(|error| format!("load_label: {error:?}"))
}

/// Intern `LABEL`, load the compiled `rules` (plus `extra` ids) and run the
/// harness task as the user under it. Returns the label id.
pub(super) fn confine_as_user(rules: &[&str], extra: &[u64]) -> Result<u32, String> {
    let label = labels::intern(LABEL).map_err(|error| format!("intern: {error:?}"))?;
    let mut ids = filepolicy::compile(rules).map_err(|(index, error)| {
        format!("rule {index} {:?}: {}", rules.get(index), error.message())
    })?;
    ids.extend_from_slice(extra);
    load_ids(label, &ids)?;
    credentials::set(
        task::KERNEL_TASK,
        Cred::new(USER_CRED.0, USER_CRED.1, 0, label, 0),
    );
    Ok(label)
}

/// The harness task as the same user with no label at all.
pub(super) fn as_plain_user() {
    credentials::set(
        task::KERNEL_TASK,
        Cred::new(USER_CRED.0, USER_CRED.1, 0, 0, 0),
    );
}

/// A NUL-terminated copy of `text`.
pub(super) fn c(text: &str) -> Vec<u8> {
    let mut out = Vec::from(text.as_bytes());
    out.push(0);
    out
}

pub(super) fn openat(path: &str, flags: u64) -> i64 {
    let path = c(path);
    process::linux::dispatch_args_for_test(257, AT_FDCWD, path.as_ptr() as u64, flags, 0o644) as i64
}

pub(super) fn openat_at(dirfd: i64, path: &str, flags: u64) -> i64 {
    let path = c(path);
    process::linux::dispatch_args_for_test(257, dirfd as u64, path.as_ptr() as u64, flags, 0o644)
        as i64
}

pub(super) fn close(fd: i64) {
    if fd >= 0 {
        let _ = process::linux::dispatch_for_test(3, fd as u64, 0, 0);
    }
}

/// Whether `open(path, flags)` succeeds, closing the descriptor again.
pub(super) fn opens(path: &str, flags: u64) -> bool {
    let fd = openat(path, flags);
    close(fd);
    fd >= 0
}

pub(super) fn stat(path: &str) -> i64 {
    let path = c(path);
    let mut buf = [0u8; 144];
    process::linux::dispatch_for_test(4, path.as_ptr() as u64, buf.as_mut_ptr() as u64, 0) as i64
}

pub(super) fn access(path: &str, mode: u64) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(21, path.as_ptr() as u64, mode, 0) as i64
}

pub(super) fn mkdir(path: &str) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(83, path.as_ptr() as u64, 0o755, 0) as i64
}

pub(super) fn rmdir(path: &str) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(84, path.as_ptr() as u64, 0, 0) as i64
}

pub(super) fn unlink(path: &str) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(87, path.as_ptr() as u64, 0, 0) as i64
}

pub(super) fn rename(from: &str, to: &str) -> i64 {
    let (from, to) = (c(from), c(to));
    process::linux::dispatch_for_test(82, from.as_ptr() as u64, to.as_ptr() as u64, 0) as i64
}

pub(super) fn chmod(path: &str, mode: u64) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(90, path.as_ptr() as u64, mode, 0) as i64
}

pub(super) fn truncate(path: &str, len: u64) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(76, path.as_ptr() as u64, len, 0) as i64
}

pub(super) fn chdir(path: &str) -> i64 {
    let path = c(path);
    process::linux::dispatch_for_test(80, path.as_ptr() as u64, 0, 0) as i64
}

pub(super) fn execve(path: &str) -> i64 {
    let path = c(path);
    let argv = [path.as_ptr() as u64, 0];
    process::linux::dispatch_for_test(59, path.as_ptr() as u64, argv.as_ptr() as u64, 0) as i64
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("confine_files_matrix", files::files_matrix),
    ("confine_creation_and_change", files::creation_and_change),
    (
        "confine_ancestors_are_only_visible",
        files::ancestors_visible,
    ),
    ("confine_dot_dot_and_dirfds", files::dot_dot_and_dirfds),
    ("confine_fabricated_entries", files::fabricated_entries),
    ("confine_execve_and_native_programs", entry::execve_refused),
    ("confine_native_filesystem_calls", entry::native_calls),
    (
        "confine_every_vfs_entry_point",
        entry::every_vfs_entry_point,
    ),
    (
        "confine_unconfined_are_untouched",
        entry::unconfined_untouched,
    ),
    ("confine_sockets", procs::sockets),
    ("confine_no_process_creation", procs::no_spawn),
    (
        "confine_inherited_descriptors_are_scrubbed",
        procs::scrub_inherited,
    ),
    (
        "confine_terminal_reads_are_eof",
        tty::terminal_reads_are_eof,
    ),
    ("confine_signals_stay_in_the_process", procs::signals),
    ("confine_dangerous_calls_stay_unimplemented", procs::enosys),
    (
        "confine_reload_revoke_and_sticky",
        policy::reload_revoke_sticky,
    ),
    (
        "confine_wildcard_rules_grant_nothing",
        policy::wildcard_rules,
    ),
    ("confine_refusals_are_audited", policy::audited),
    ("confine_soak", policy::soak),
];

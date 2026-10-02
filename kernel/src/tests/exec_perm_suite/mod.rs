//! The execute bit on program start (issue #507, section 5): `spawnv` under
//! both personalities and Linux `execve` all refuse a file without an `x`
//! bit for the caller (root included), a directory, and anything on a
//! `noexec` mount, and they run a `0755` file.
//!
//! Each test swaps in its own native and Linux ABI mount tables, sharing one
//! set of ramfs instances like a configured boot does: `/transient` (plain),
//! `/mnt` (`noexec`). A successful `execve` would replace the harness's own
//! image, so the Linux success case drives `shebang::resolve`, everything
//! `execve` does before it builds the address space.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{FsError, Id, MountFlags, Vfs};
use crate::ipc::credentials::{self, Cred};
use crate::process::linux::shebang;
use alloc::sync::Arc;

mod soak;

const ENOENT: i64 = 2;
const EACCES: i64 = 13;
const SYS_EXECVE: u64 = 59;
const SYS_SPAWNV: u64 = 30;

/// The files every test sees. All are root-owned.
const PLAIN: &str = "/transient/plain"; // 0644
const PRIVATE: &str = "/transient/private"; // 0700
const RUN: &str = "/transient/run"; // 0755
const DIR: &str = "/transient/dir"; // directory, 0755
const LOCKED: &str = "/mnt/run"; // 0755 on the noexec mount
const MISSING: &str = "/transient/missing";

/// A user with no relation to the files' owner (root).
const USER: Cred = Cred::new(1000, 100, 0, 0, 0);

fn fs_error(error: FsError) -> String {
    String::from(error.message())
}

/// One mount table over the shared volumes, with the files of this suite.
fn table(root: &Arc<RamFs>, transient: &Arc<RamFs>, locked: &Arc<RamFs>) -> Result<Vfs, String> {
    let noexec = MountFlags {
        noexec: true,
        ..MountFlags::default()
    };
    let mut vfs = Vfs::new();
    vfs.mount(fhs::mount::ROOT, root.clone(), MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount(
        fhs::mount::TRANSIENT,
        transient.clone(),
        MountFlags::default(),
    )
    .map_err(fs_error)?;
    vfs.mount("/mnt", locked.clone(), noexec)
        .map_err(fs_error)?;
    vfs.set_umask(0);
    Ok(vfs)
}

fn populate(vfs: &mut Vfs) -> Result<(), String> {
    let elf = service_suite::minimal_elf();
    for (path, mode) in [
        (PLAIN, 0o644),
        (PRIVATE, 0o700),
        (RUN, 0o755),
        (LOCKED, 0o755),
    ] {
        vfs.create(Id::ROOT, path, mode).map_err(fs_error)?;
        vfs.write(Id::ROOT, path, 0, &elf).map_err(fs_error)?;
    }
    vfs.mkdir(Id::ROOT, DIR, 0o755).map_err(fs_error)?;
    Ok(())
}

/// Run `body` as the kernel task with this suite's mount tables installed,
/// then put the previous tables and root credentials back.
fn with_tables(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    let volumes = [
        Arc::new(RamFs::new()),
        Arc::new(RamFs::new()),
        Arc::new(RamFs::new()),
    ];
    let mut native = table(&volumes[0], &volumes[1], &volumes[2])?;
    populate(&mut native)?;
    let abi = table(&volumes[0], &volumes[1], &volumes[2])?;
    let previous_native = crate::fs::install_native_for_test(native);
    let previous_abi = crate::fs::install_abi_for_test(abi);
    let result = body();
    crate::fs::restore_abi_for_test(previous_abi);
    crate::fs::restore_native_for_test(previous_native);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    task::harness::reset();
    result
}

fn c(text: &str) -> Vec<u8> {
    let mut out = Vec::from(text.as_bytes());
    out.push(0);
    out
}

/// Spawn `line` (a path, `linux:` in front for the Linux personality)
/// through `spawnv` with `argv` `[path]`; a started child is finished and
/// reaped at once, so the result is the pid or the negative errno.
fn spawn(line: &str) -> i64 {
    use crate::process::spawnv::{personality, REQ_WORDS};
    let (path, linux) = match line.strip_prefix("linux:") {
        Some(path) => (path, true),
        None => (line, false),
    };
    let argv = c(path);
    let mut words = [0u64; REQ_WORDS];
    words[..5].copy_from_slice(&[
        path.as_ptr() as u64,
        path.len() as u64,
        argv.as_ptr() as u64,
        argv.len() as u64,
        1,
    ]);
    words[8] = if linux {
        personality::LINUX
    } else {
        personality::NATIVE
    };
    let code = process::dispatch_for_test(SYS_SPAWNV, words.as_ptr() as u64, 0, 0) as i64;
    if code > 0 {
        task::harness::finish(code as usize, 0);
        let _ = task::reap_child_slot(code as usize);
    }
    code
}

/// `execve(path, [path], [])` through the Linux syscall: `-errno`.
fn execve(path: &str) -> i64 {
    let path = c(path);
    let argv = [path.as_ptr() as u64, 0];
    process::linux::dispatch_for_test(SYS_EXECVE, path.as_ptr() as u64, argv.as_ptr() as u64, 0)
        as i64
}

/// Whether `execve` would load `path` (everything short of replacing the image).
fn execve_resolves(path: &str) -> bool {
    shebang::resolve(
        String::from(path),
        Vec::from(path.as_bytes()),
        vec![c(path)],
    )
    .is_ok()
}

/// What each identity gets for each file: `(path, as user, as root)`.
const MATRIX: &[(&str, i64, i64)] = &[
    (PLAIN, -EACCES, -EACCES),
    (PRIVATE, -EACCES, 0),
    (RUN, 0, 0),
    (DIR, -EACCES, -EACCES),
    (LOCKED, -EACCES, -EACCES),
    (MISSING, -ENOENT, -ENOENT),
];

/// Compare a spawn result against the matrix: `0` there means "started".
fn matches(got: i64, want: i64) -> bool {
    if want == 0 {
        got > 0
    } else {
        got == want
    }
}

/// The matrix through native `spawn` and a `linux:` spawn line.
pub fn exec_perm_spawn_matrix() -> Result<(), String> {
    with_tables(|| {
        for (cred, label) in [(USER, "uid 1000"), (Cred::ROOT, "root")] {
            credentials::set(task::KERNEL_TASK, cred);
            for &(path, user, root) in MATRIX {
                let want = if cred.uid == 0 { root } else { user };
                let native = spawn(path);
                check!(
                    matches(native, want),
                    "{label}: native spawn {path} gave {native}, want {want}"
                );
                let linux = spawn(&format!("linux:{path}"));
                check!(
                    matches(linux, want),
                    "{label}: linux spawn {path} gave {linux}, want {want}"
                );
            }
        }
        Ok(())
    })
}

/// The same matrix through Linux `execve`.
pub fn exec_perm_execve_matrix() -> Result<(), String> {
    with_tables(|| {
        for (cred, label) in [(USER, "uid 1000"), (Cred::ROOT, "root")] {
            credentials::set(task::KERNEL_TASK, cred);
            for &(path, user, root) in MATRIX {
                let want = if cred.uid == 0 { root } else { user };
                if want == 0 {
                    check!(execve_resolves(path), "{label}: execve {path} refused");
                } else {
                    let got = execve(path);
                    check!(
                        got == want,
                        "{label}: execve {path} gave {got}, want {want}"
                    );
                }
            }
        }
        Ok(())
    })
}

/// Root's one exception to the bypass: an `x` bit for anyone is enough, and
/// directory search keeps the full bypass (root walks a `0000` directory,
/// which nobody else may search).
pub fn exec_perm_root_needs_one_x_bit() -> Result<(), String> {
    with_tables(|| {
        let id = Id::ROOT;
        for (mode, runs) in [(0o001, true), (0o010, true), (0o100, true), (0o666, false)] {
            let path = format!("/transient/m{mode:o}");
            crate::fs::vfs_create(id, &path, mode).map_err(fs_error)?;
            let elf = service_suite::minimal_elf();
            crate::fs::vfs_write(id, &path, 0, &elf).map_err(fs_error)?;
            let got = spawn(&path);
            check!(
                matches(got, if runs { 0 } else { -EACCES }),
                "root spawn of a {mode:o} file gave {got}"
            );
        }
        crate::fs::vfs_mkdir(id, "/transient/x", 0o000).map_err(fs_error)?;
        crate::fs::vfs_create(id, "/transient/x/run", 0o755).map_err(fs_error)?;
        let elf = service_suite::minimal_elf();
        crate::fs::vfs_write(id, "/transient/x/run", 0, &elf).map_err(fs_error)?;
        let got = spawn("/transient/x/run");
        check!(
            got > 0,
            "root could not run a file in a 0000 directory: {got}"
        );
        credentials::set(task::KERNEL_TASK, USER);
        let got = spawn("/transient/x/run");
        check!(
            got == -EACCES,
            "a user ran a file in a 0000 directory: {got}"
        );
        Ok(())
    })
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("exec_perm_spawn_matrix", exec_perm_spawn_matrix),
    ("exec_perm_execve_matrix", exec_perm_execve_matrix),
    (
        "exec_perm_root_needs_one_x_bit",
        exec_perm_root_needs_one_x_bit,
    ),
    ("exec_perm_soak_denied", soak::denied_spawns_leak_nothing),
    ("exec_perm_soak_allowed", soak::allowed_spawns_leak_nothing),
];

//! `spawnv` (syscall 31, fs F3 / issue #507): the argv-vector spawn.
//!
//! The harness does not run user code, so the suite checks what a child
//! would find when it starts: a native child's `argv`/`envp` blocks through
//! syscall 9 issued as that child, a Linux child's start stack read through
//! its own page table. The programs are the minimal service ELF, written to
//! `/tmp` under paths that contain spaces.
//!
//! * [`valid`]: argv/env reach both personalities unchanged, credential
//!   stamps, a boot spawn's `argv` vector;
//! * [`errors`]: every limit and malformed request returns its errno and
//!   leaks nothing, the credential modes refuse an unprivileged caller;
//! * [`soak`]: 10 000 spawn/exit cycles and 10 000 refusals leave frames,
//!   memory, argument blocks and interned names at their baseline.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::process::spawnv::{cred_mode, personality, REQ_WORDS};

mod errors;
mod soak;
mod valid;

use errors::*;
use soak::*;
use valid::*;

const SYS_ARGS: u64 = 9;
const SYS_WAIT: u64 = 7;
const SYS_SPAWNV: u64 = 31;

/// The directory holding the suite's programs; the space is deliberate.
const DIR: &str = "/tmp/spawn suite";
/// A program path with spaces, spawned as native.
const NATIVE: &str = "/tmp/spawn suite/native prog";
/// A program path with spaces, spawned as Linux.
const LINUX: &str = "/tmp/spawn suite/linux prog";

/// `-errno` as the syscall returns it.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// The bring-up state: root kernel task current, every other slot free, the
/// boot volumes mounted, and the suite's programs installed (0755).
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
    crate::fs::init();
    let id = crate::fs::vfs::Id::ROOT;
    let _ = crate::fs::vfs_mkdir(id, DIR, 0o755);
    let elf = service_suite::minimal_elf();
    for path in [NATIVE, LINUX] {
        // Present from an earlier test is fine; the write must succeed.
        let _ = crate::fs::vfs_create(id, path, 0o755);
        crate::fs::vfs_write(id, path, 0, &elf).map_err(|e| format!("{path}: {}", e.message()))?;
    }
    Ok(())
}

/// One `spawnv` request, held as kernel buffers (the harness trusts kernel
/// pointers unless a test turns validation on).
#[derive(Clone)]
struct Req {
    path: Vec<u8>,
    argv: Vec<u8>,
    argc: u64,
    envp: Vec<u8>,
    envc: u64,
    personality: u64,
    mode: u64,
    cred: Cred,
    label: Vec<u8>,
}

impl Req {
    /// A well-formed request for `path`.
    fn new(path: &str, argv: &[&[u8]], envp: &[&[u8]], linux: bool) -> Req {
        Req {
            path: path.as_bytes().to_vec(),
            argv: block(argv),
            argc: argv.len() as u64,
            envp: block(envp),
            envc: envp.len() as u64,
            personality: if linux {
                personality::LINUX
            } else {
                personality::NATIVE
            },
            mode: cred_mode::INHERIT,
            cred: Cred::new(0, 0, 0, 0, 0),
            label: Vec::new(),
        }
    }

    /// The request block, pointing at this request's buffers.
    fn words(&self) -> [u64; REQ_WORDS] {
        let cred = self.cred.to_words();
        [
            self.path.as_ptr() as u64,
            self.path.len() as u64,
            self.argv.as_ptr() as u64,
            self.argv.len() as u64,
            self.argc,
            self.envp.as_ptr() as u64,
            self.envp.len() as u64,
            self.envc,
            self.personality,
            self.mode,
            cred[0],
            cred[1],
            cred[2],
            cred[3],
            cred[4],
            self.label.as_ptr() as u64,
            self.label.len() as u64,
        ]
    }

    /// Issue the syscall.
    fn call(&self) -> u64 {
        let words = self.words();
        process::dispatch_for_test(SYS_SPAWNV, words.as_ptr() as u64, 0, 0)
    }
}

/// Each item followed by a NUL.
fn block(items: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in items {
        bytes.extend_from_slice(item);
        bytes.push(0);
    }
    bytes
}

/// The slot a successful spawn returned, or the failure.
fn spawned(code: u64) -> Result<usize, String> {
    if (1..task::MAX_TASKS as u64).contains(&code) {
        Ok(code as usize)
    } else {
        Err(format!("spawnv returned {code:#x}"))
    }
}

/// Block `which` (0 argv, 1 envp) as the child in `slot` reads it through
/// syscall 9.
fn child_block(slot: usize, which: u64) -> Vec<u8> {
    task::harness::switch_current(slot);
    let len = process::dispatch_for_test(SYS_ARGS, 0, 0, which);
    let mut buf = vec![0u8; len as usize];
    let copied = process::dispatch_for_test(SYS_ARGS, buf.as_mut_ptr() as u64, len, which);
    task::harness::switch_current(task::KERNEL_TASK);
    buf.truncate(copied.min(len) as usize);
    buf
}

/// The `argv` and `envp` strings on the start stack of the Linux child in
/// `slot`, read through its own page table.
fn linux_start_stack(slot: usize) -> Result<(Vec<Vec<u8>>, Vec<Vec<u8>>), String> {
    let rsp = task::harness::frame_regs(slot).ok_or("no frame")?.rsp;
    let pml4 = task::harness::pml4(slot).ok_or("no address space")?;
    // `kernel_table` reads CR3, so take it before switching away.
    let kernel = mem::kernel_table();
    mem::switch_to(PhysAddr::new(pml4));
    // SAFETY: the child's table is installed; its start stack (argc, the
    // argv and envp pointer arrays and their strings) was mapped and written
    // by the loader, and `read_strings` stops at each array's NULL.
    let stack = unsafe {
        let argc = core::ptr::read_volatile(rsp as *const u64);
        let argv = read_strings(rsp + 8, Some(argc));
        let envp = read_strings(rsp + 8 * (argc + 2), None);
        (argv, envp)
    };
    mem::switch_to(kernel);
    Ok(stack)
}

/// Read a NULL-terminated array of C string pointers at `at` (at most
/// `count` entries when given).
///
/// # Safety
/// The array and its strings must be mapped in the installed table.
unsafe fn read_strings(at: u64, count: Option<u64>) -> Vec<Vec<u8>> {
    let mut items = Vec::new();
    for index in 0..count.unwrap_or(256) {
        // SAFETY: the caller guarantees the array is mapped.
        let ptr = unsafe { core::ptr::read_volatile((at + 8 * index) as *const u64) };
        if ptr == 0 {
            break;
        }
        let mut item = Vec::new();
        for offset in 0..4096u64 {
            // SAFETY: the caller guarantees the string is mapped.
            let byte = unsafe { core::ptr::read_volatile((ptr + offset) as *const u8) };
            if byte == 0 {
                break;
            }
            item.push(byte);
        }
        items.push(item);
    }
    items
}

/// Finish the child in `slot` and reap it through `wait`.
fn reap(slot: usize) -> Result<(), String> {
    task::harness::finish(slot, 0);
    let packed = process::dispatch_for_test(SYS_WAIT, 0, 0, 0);
    check!(
        packed != u64::MAX && packed >> 32 == slot as u64,
        "wait returned {packed:#x} for slot {slot}"
    );
    Ok(())
}

/// What a spawn could leak: live frames, kernel memory (slab + heap), slots
/// holding argument blocks, and occupied task slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Usage {
    frames: usize,
    bytes: usize,
    arg_blocks: usize,
    tasks: usize,
}

fn usage() -> Usage {
    Usage {
        frames: mem::frame_stats().live(),
        bytes: crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used,
        arg_blocks: process::task_args_live_for_test(),
        tasks: (1..task::MAX_TASKS)
            .filter(|slot| task::harness::state(*slot).is_some())
            .count(),
    }
}

/// `after` is back at `before`: exact for frames, blocks and slots, within
/// `slack` bytes for the allocators (slab caches keep partial pages).
fn no_leak(before: Usage, after: Usage, slack: usize, what: &str) -> Result<(), String> {
    check!(
        after.frames <= before.frames
            && after.arg_blocks <= before.arg_blocks
            && after.tasks <= before.tasks
            && after.bytes <= before.bytes + slack,
        "{what}: leaked: {before:?} -> {after:?}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "spawn_native_argv_env_reach_child",
        native_argv_env_reach_child,
    ),
    (
        "spawn_linux_argv_env_reach_child",
        linux_argv_env_reach_child,
    ),
    ("spawn_path_with_space_spawns", path_with_space_spawns),
    ("spawn_cred_stamps_child", cred_stamps_child),
    (
        "spawn_boot_spawn_argv_is_a_vector",
        boot_spawn_argv_is_a_vector,
    ),
    ("spawn_limit_errors_leak_nothing", limit_errors_leak_nothing),
    ("spawn_malformed_requests_einval", malformed_requests_einval),
    ("spawn_unmapped_memory_efault", unmapped_memory_efault),
    (
        "spawn_cred_without_capability_eperm",
        cred_without_capability_eperm,
    ),
    ("spawn_soak_spawnv_exit_cycles", soak_spawnv_exit_cycles),
    ("spawn_soak_denied_spawns", soak_denied_spawns),
];

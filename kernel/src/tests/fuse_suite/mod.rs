//! User-space filesystems (`fs::fuse`, docs/smb-plan.md F1): the real
//! kernel path (VFS -> backend -> slot -> provider) with a fake daemon that
//! the waiting requester runs in place of parking, serving `libs/fused`'s
//! in-memory tree through `fused::daemon::serve_one` (the code `memfuse`
//! runs); hostile daemons (lying replies, silence, death); the syscall 35
//! gate; and a soak.

mod hostile;
mod io;
mod sys;

use super::*;
use crate::fs::fuse::{self, test_hook, FuseError};
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{Filesystem, FsError, Id, MountFlags, Vfs};
use crate::ipc::credentials::{self, Cred, CAP_FS_PROVIDER};
use alloc::sync::Arc;
use fused::daemon::{serve_one, Buffers, Provider};
use fused::memfs::MemFs;
use fused::wire::{Reply, Request, MAX_PAYLOAD};
use spin::Mutex;

/// The test daemon's uid.
const DAEMON_UID: u32 = 1000;

/// How the fake daemon answers its next requests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Serve every request from the tree.
    Normal,
    /// Answer every request with this errno.
    Status(u64),
    /// Take a request and never answer (the clock moves on).
    Silent,
    /// Answer with a tag that is not the request's.
    WrongTag,
    /// Take the request, then die (`teardown_task`).
    DieAfterTake,
    /// Answer with more data than the request has room for.
    Oversize,
    /// Answer with data the kernel cannot copy (a faulting buffer).
    FaultData,
    /// Serve from the tree, then rewrite the reply with `Lie`.
    Lie(Lie),
}

/// A reply rewritten after the tree answered it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lie {
    /// A node type the VFS has no word for (a symlink).
    Symlink,
    /// A uid that does not fit 32 bits.
    HugeUid,
    /// A read whose count is larger than its data.
    ReadCount,
    /// A directory payload that is garbage.
    Dirents,
    /// A listing that never ends: every batch claims one more entry.
    EndlessDir,
}

struct Fake {
    index: usize,
    owner: usize,
    mode: Mode,
    fs: MemFs,
    served: u64,
    /// Op codes of the requests taken, oldest first.
    ops: Vec<u64>,
    buffers: Buffers,
}

static FAKE: Mutex<Option<Fake>> = Mutex::new(None);

fn clock() -> i64 {
    1_700_000_000
}

/// One request already taken from the slot, handed to `serve_one` as if it
/// came from `NEXT`; its reply goes to the kernel (or to `rewrite` first).
struct Taken {
    index: usize,
    owner: usize,
    request: Option<Request>,
    payload: Vec<u8>,
    lie: Option<Lie>,
    /// The op code of the request, which decides whether the lie applies.
    op: u64,
}

impl Provider for Taken {
    fn next(&mut self, buf: &mut [u8]) -> Result<Option<Request>, i64> {
        buf[..self.payload.len()].copy_from_slice(&self.payload);
        Ok(self.request.take())
    }

    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64> {
        let mut reply = *reply;
        let mut data = data.to_vec();
        if let Some(lie) = self.lie.filter(|lie| lie.applies_to(self.op)) {
            rewrite(lie, &mut reply, &mut data);
        }
        fuse::reply(self.index, self.owner, &reply, &mut |bounce| {
            bounce.copy_from_slice(&data[..bounce.len()]);
            Ok(())
        })
        .map_err(|_| 1)
    }
}

impl Lie {
    /// Only the op a lie is about is rewritten; the lookups around it stay
    /// honest, so the lie is what the kernel trips on.
    fn applies_to(self, op: u64) -> bool {
        let want = match self {
            Lie::Symlink | Lie::HugeUid => fused::wire::Op::Lookup,
            Lie::ReadCount => fused::wire::Op::Read,
            Lie::Dirents | Lie::EndlessDir => fused::wire::Op::ReadDir,
        };
        op & 0xFFFF == want as u64
    }
}

fn rewrite(lie: Lie, reply: &mut Reply, data: &mut Vec<u8>) {
    match lie {
        Lie::Symlink => reply.attr.mode = 0o120777,
        Lie::HugeUid => reply.attr.uid = u64::from(u32::MAX) + 1,
        Lie::ReadCount => reply.count += 1,
        Lie::Dirents => {
            data.clear();
            data.extend_from_slice(&[0xFF; 24]);
            reply.count = 2;
            reply.data_len = 24;
        }
        Lie::EndlessDir => {
            // A full batch of entries, every time, whatever the index.
            data.clear();
            data.resize(fused::wire::MAX_DATA, 0);
            let (mut at, mut count) = (0, 0);
            while let Some(next) = fused::payload::encode_dirent(data, at, 7, false, "x") {
                at = next;
                count += 1;
            }
            data.truncate(at);
            reply.status = 0;
            reply.count = count;
            reply.data_len = at as u64;
        }
    }
}

/// The fake daemon: take the queued request (if any) and answer it.
fn serve(index: usize) {
    let mut guard = FAKE.lock();
    let Some(fake) = guard.as_mut() else {
        return;
    };
    if fake.index != index {
        return;
    }
    let mut payload = Vec::new();
    let taken = fuse::next(index, fake.owner, 0, &mut |bytes| {
        payload.extend_from_slice(bytes);
        Ok(())
    });
    let Ok(Some(request)) = taken else {
        test_hook::advance(fuse::SLICE_TICKS);
        return;
    };
    fake.served += 1;
    fake.ops.push(request.op & 0xFFFF);
    let answer = |status: u64, data_len: u64| Reply {
        tag: request.tag,
        status,
        data_len,
        ..Reply::default()
    };
    match fake.mode {
        Mode::Silent => test_hook::advance(fuse::SLICE_TICKS),
        Mode::DieAfterTake => fuse::teardown_task(fake.owner),
        Mode::WrongTag => {
            let mut wrong = answer(0, 0);
            wrong.tag ^= 1;
            let refused = fuse::reply(index, fake.owner, &wrong, &mut |_| Ok(()));
            assert!(refused == Err(FuseError::Stale), "a wrong tag was accepted");
            test_hook::advance(fuse::SLICE_TICKS);
        }
        Mode::Status(code) => {
            let done = fuse::reply(index, fake.owner, &answer(code, 0), &mut |_| Ok(()));
            assert!(done.is_ok(), "a status reply was refused: {done:?}");
        }
        Mode::Oversize => {
            let big = answer(0, MAX_PAYLOAD as u64 + 1);
            let refused = fuse::reply(index, fake.owner, &big, &mut |_| Ok(()));
            assert!(refused == Err(FuseError::Invalid), "oversized: {refused:?}");
        }
        Mode::FaultData => {
            let reply = answer(0, 4);
            let refused = fuse::reply(index, fake.owner, &reply, &mut |_| Err(FuseError::Fault));
            assert!(
                refused == Err(FuseError::Fault),
                "faulting data: {refused:?}"
            );
        }
        Mode::Normal | Mode::Lie(_) => {
            let lie = match fake.mode {
                Mode::Lie(lie) => Some(lie),
                _ => None,
            };
            let mut provider = Taken {
                index,
                owner: fake.owner,
                request: Some(request),
                payload,
                lie,
                op: request.op,
            };
            let served = serve_one(&mut fake.fs, &mut provider, &mut fake.buffers);
            assert!(served == Ok(true), "serve_one: {served:?}");
        }
    }
}

/// A clean task table with only the kernel task, current.
fn fresh_tasks() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
}

/// A daemon task (`CAP_FS_PROVIDER`), made current: it is the requester too.
fn daemon_task() -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    credentials::set(
        slot,
        Cred::new(DAEMON_UID, DAEMON_UID, CAP_FS_PROVIDER, 0, 0),
    );
    task::harness::switch_current(slot);
    Ok(slot)
}

/// A mount table over a ramfs root holding `/mnt` (or not).
fn table(with_mnt: bool) -> Vfs {
    let root = RamFs::new();
    if with_mnt {
        let _ = root.mkdir("mnt", 0o755, Id::ROOT);
    }
    let mut vfs = Vfs::new();
    let _ = vfs.mount(fhs::mount::ROOT, Arc::new(root), MountFlags::default());
    vfs
}

/// The tables a test replaced, put back by [`teardown`].
struct Saved {
    native: Option<(Vfs, bool)>,
    abi: Option<Vfs>,
}

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

/// Fresh tables, a daemon task and the fake daemon (not yet mounted).
fn prepare(with_mnt: bool) -> Result<usize, String> {
    fresh_tasks();
    test_hook::recycle_all();
    let native = crate::fs::install_native_for_test(table(with_mnt));
    let abi = crate::fs::install_abi_for_test(table(with_mnt));
    *SAVED.lock() = Some(Saved { native, abi });
    daemon_task()
}

/// [`prepare`], then mount `/mnt/<name>` served by the fake in `mode`.
fn setup_named(name: &str, mode: Mode, flags: MountFlags) -> Result<usize, String> {
    let owner = prepare(true)?;
    let index = fuse::register(owner, name, flags).map_err(|e| format!("register: {e:?}"))?;
    *FAKE.lock() = Some(Fake {
        index,
        owner,
        mode,
        fs: MemFs::new(4 << 20, 4100, DAEMON_UID, DAEMON_UID, clock),
        served: 0,
        ops: Vec::new(),
        buffers: Buffers::new(),
    });
    test_hook::set_server(Some(serve));
    Ok(index)
}

fn setup(mode: Mode) -> Result<usize, String> {
    setup_named("t", mode, MountFlags::default())
}

/// Undo [`setup`]: slots, fake, tables, tasks.
fn teardown() {
    test_hook::recycle_all();
    *FAKE.lock() = None;
    if let Some(saved) = SAVED.lock().take() {
        crate::fs::restore_native_for_test(saved.native);
        crate::fs::restore_abi_for_test(saved.abi);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
}

fn mode(mode: Mode) {
    if let Some(fake) = FAKE.lock().as_mut() {
        fake.mode = mode;
    }
}

fn with_fake<T>(f: impl FnOnce(&mut Fake) -> T) -> T {
    f(FAKE.lock().as_mut().expect("fake daemon"))
}

/// A deterministic pattern.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ (i >> 8) as u8 ^ seed)
        .collect()
}

fn expect<T: core::fmt::Debug>(
    result: Result<T, FsError>,
    want: FsError,
    what: &str,
) -> Result<(), String> {
    match result {
        Err(error) if error == want => Ok(()),
        other => Err(format!("{what}: {other:?}, wanted {want:?}")),
    }
}

/// Run `body` between [`setup`] in `mode` and [`teardown`].
fn run(mode: Mode, body: impl FnOnce(usize) -> Result<(), String>) -> Result<(), String> {
    let index = setup(mode)?;
    let result = body(index);
    teardown();
    result
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("fuse_roundtrip_both_tables", io::roundtrip_both_tables),
    ("fuse_tree_operations", io::tree_operations),
    ("fuse_nodes_follow_renames", io::nodes_follow_renames),
    ("fuse_open_file_in_place", io::open_file_in_place),
    ("fuse_large_directory", io::large_directory),
    ("fuse_read_only_mount", io::read_only_mount),
    ("fuse_unregister_and_remount", io::unregister_and_remount),
    ("fuse_stress", io::stress),
    ("fuse_error_statuses", hostile::error_statuses),
    ("fuse_silent_daemon_dies", hostile::silent_daemon_dies),
    ("fuse_death_mid_request", hostile::death_mid_request),
    ("fuse_stale_reply", hostile::stale_reply),
    ("fuse_oversized_reply", hostile::oversized_reply),
    ("fuse_faulting_reply_data", hostile::faulting_reply_data),
    ("fuse_lying_replies", hostile::lying_replies),
    ("fuse_endless_directory", hostile::endless_directory),
    ("fuse_sys_gate", sys::gate),
    ("fuse_sys_request_cycle", sys::request_cycle),
    ("fuse_sys_hostile_buffers", sys::hostile_buffers),
];

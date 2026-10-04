//! Syscall 35: who may provide, names and flags, and the REGISTER / NEXT /
//! REPLY cycle driven through the syscall surface with hostile records and
//! buffers.

use super::*;
use crate::fs as vfsapi;
use crate::fs::fuse::sys as fusesys;
use crate::user_ptr;
use fused::wire::{sys_op, REPLY_WORDS, REQUEST_WORDS};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const ESRCH: i64 = 3;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const ENOSPC: i64 = 28;
const ESTALE: i64 = 116;

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn call(operation: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    fusesys::dispatch(operation, a1, a2, a3, a4)
}

/// REGISTER: through the gate's own test entry, like a real one, when the
/// flags are 0 (the entry has no fourth argument).
fn register_call(name: &[u8], flags: u64) -> u64 {
    let (address, len) = (name.as_ptr() as u64, name.len() as u64);
    if flags == 0 {
        process::dispatch_for_test(35, sys_op::REGISTER, address, len)
    } else {
        call(sys_op::REGISTER, address, len, flags, 0)
    }
}

/// A task with `uid` and `caps`, made current.
fn task_with(uid: u32, caps: u32) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    credentials::set(slot, Cred::new(uid, uid, caps, 0, 0));
    task::harness::switch_current(slot);
    Ok(slot)
}

pub fn gate() -> Result<(), String> {
    prepare(true)?;
    let result = (|| {
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            register_call(b"k", 0) == failed(EPERM),
            "the kernel task registered"
        );
        task_with(0, 0)?;
        check!(
            register_call(b"k", 0) == failed(EPERM),
            "a task without the capability"
        );
        task_with(DAEMON_UID, CAP_FS_PROVIDER)?;
        for bad in [
            &b""[..],
            b".",
            b"..",
            b"a/b",
            b"a b",
            &[b'n'; 65][..],
            &[0xFF][..],
        ] {
            check!(
                register_call(bad, 0) == failed(EINVAL),
                "the name {bad:?} was taken"
            );
        }
        check!(register_call(b"x", 4) == failed(EINVAL), "unknown flags");
        let previous = user_ptr::set_trust_kernel_pointers(false);
        let null = call(sys_op::REGISTER, 0, 3, 0, 0);
        user_ptr::set_trust_kernel_pointers(previous);
        check!(null == failed(EFAULT), "a null name: {null:#x}");
        check!(call(9, 0, 0, 0, 0) == failed(EINVAL), "an unknown op");
        for id in 0..fuse::MAX_PROVIDERS as u64 {
            let name = alloc::format!("p{id}");
            let got = register_call(
                name.as_bytes(),
                if id == 1 { fused::wire::FLAG_RO } else { 0 },
            );
            check!(got == id, "registration {id} returned {got:#x}");
        }
        check!(
            register_call(b"p9", 0) == failed(ENOSPC),
            "a ninth provider"
        );
        check!(
            vfsapi::abi_mount_flags("/mnt/p1/x").ro && !vfsapi::abi_mount_flags("/mnt/p0/x").ro,
            "FLAG_RO did not reach the mount"
        );
        // A provider answers only the task that registered it.
        let me = task::current();
        task_with(DAEMON_UID, CAP_FS_PROVIDER)?;
        let mut req = [0u64; REQUEST_WORDS];
        let next = call(sys_op::NEXT, 0, req.as_mut_ptr() as u64, 0, 0);
        check!(
            next == failed(ESRCH),
            "another task took a request: {next:#x}"
        );
        check!(
            call(sys_op::UNREGISTER, 0, 0, 0, 0) == failed(ESRCH),
            "another task unmounted"
        );
        task::harness::switch_current(me);
        check!(
            call(sys_op::UNREGISTER, 0, 0, 0, 0) == 0,
            "the owner could not unmount"
        );
        check!(
            register_call(b"p1", 0) == failed(EBUSY),
            "a live name taken twice"
        );
        check!(register_call(b"p0", 0) == 0, "a freed name and slot");
        check!(
            call(sys_op::UNREGISTER, u64::MAX, 0, 0, 0) == failed(ESRCH),
            "no such id"
        );
        Ok(())
    })();
    teardown();
    result?;
    // Without `/mnt` on the system there is nowhere to mount.
    prepare(false)?;
    let missing = register_call(b"m", 0);
    teardown();
    check!(
        missing == failed(ENOENT),
        "mounted without /mnt: {missing:#x}"
    );
    Ok(())
}

/// How the syscall-level fake daemon misbehaves on its next turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Trick {
    None,
    /// NEXT with a payload buffer too small for the request.
    SmallCap,
    /// NEXT with an unwritable request record.
    BadRecord,
    /// REPLY with an unreadable reply record.
    BadReply,
}

static TRICK: Mutex<(Trick, Vec<u64>)> = Mutex::new((Trick::None, Vec::new()));

fn trick(trick: Trick) {
    TRICK.lock().0 = trick;
}

/// Return codes the fake daemon saw, oldest first.
fn take_results() -> Vec<u64> {
    core::mem::take(&mut TRICK.lock().1)
}

/// `serve_one`'s provider over syscall 35, with the current trick.
struct SysProvider {
    id: u64,
    trick: Trick,
}

impl Provider for SysProvider {
    fn next(&mut self, payload: &mut [u8]) -> Result<Option<Request>, i64> {
        let mut record = [0u64; REQUEST_WORDS];
        let cap = if self.trick == Trick::SmallCap {
            1
        } else {
            payload.len() as u64
        };
        let previous = user_ptr::set_trust_kernel_pointers(self.trick != Trick::BadRecord);
        let address = if self.trick == Trick::BadRecord {
            0x10
        } else {
            record.as_mut_ptr() as u64
        };
        let got = call(
            sys_op::NEXT,
            self.id,
            address,
            payload.as_mut_ptr() as u64,
            cap,
        );
        user_ptr::set_trust_kernel_pointers(previous);
        TRICK.lock().1.push(got);
        match got {
            1 => Ok(Some(Request::from_words(&record))),
            0 => Ok(None),
            code => Err(code as i64),
        }
    }

    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64> {
        let record: [u64; REPLY_WORDS] = reply.to_words();
        let bad = self.trick == Trick::BadReply;
        let previous = user_ptr::set_trust_kernel_pointers(!bad);
        let address = if bad { 0x10 } else { record.as_ptr() as u64 };
        let done = call(sys_op::REPLY, self.id, address, data.as_ptr() as u64, 0);
        user_ptr::set_trust_kernel_pointers(previous);
        TRICK.lock().1.push(done);
        Ok(())
    }
}

/// The fake daemon, through syscall 35.
fn serve_sys(index: usize) {
    let trick = core::mem::replace(&mut TRICK.lock().0, Trick::None);
    let mut guard = FAKE.lock();
    let Some(fake) = guard.as_mut() else {
        return;
    };
    let mut provider = SysProvider {
        id: index as u64,
        trick,
    };
    match serve_one(&mut fake.fs, &mut provider, &mut fake.buffers) {
        Ok(true) => fake.served += 1,
        _ => test_hook::advance(fuse::SLICE_TICKS),
    }
}

fn sys_setup() -> Result<u64, String> {
    let owner = prepare(true)?;
    let id = register_call(b"s", 0);
    check!(id < fuse::MAX_PROVIDERS as u64, "REGISTER: {id:#x}");
    *FAKE.lock() = Some(Fake {
        index: id as usize,
        owner,
        mode: Mode::Normal,
        fs: MemFs::new(1 << 20, 64, DAEMON_UID, DAEMON_UID, clock),
        served: 0,
        ops: Vec::new(),
        buffers: Buffers::new(),
    });
    test_hook::set_server(Some(serve_sys));
    trick(Trick::None);
    take_results();
    Ok(id)
}

pub fn request_cycle() -> Result<(), String> {
    let id = sys_setup()?;
    let result = (|| {
        let root = Id::ROOT;
        vfsapi::abi_create(root, "/mnt/s/f", 0o644).map_err(|e| format!("create: {e:?}"))?;
        let body = pattern(70_000, 9);
        check!(
            vfsapi::abi_write(root, "/mnt/s/f", 0, &body) == Ok(body.len()),
            "write"
        );
        let back = vfsapi::abi_read(root, "/mnt/s/f").map_err(|e| format!("read: {e:?}"))?;
        check!(back == body, "read back other bytes");
        let names = vfsapi::abi_readdir(root, "/mnt/s").map_err(|e| format!("readdir: {e:?}"))?;
        check!(
            names.len() == 1 && names[0].name == "f",
            "listing: {names:?}"
        );
        let results = take_results();
        check!(
            !results.is_empty() && results.chunks(2).all(|pair| pair == [1, 0]),
            "the daemon saw {results:x?}"
        );
        // Nothing queued: NEXT reports none, REPLY is stale.
        let mut req = [0u64; REQUEST_WORDS];
        let next = call(sys_op::NEXT, id, req.as_mut_ptr() as u64, 0, 0);
        check!(next == 0, "NEXT with nothing queued: {next:#x}");
        let reply = [0u64; REPLY_WORDS];
        let stale = call(sys_op::REPLY, id, reply.as_ptr() as u64, 0, 0);
        check!(
            stale == failed(ESTALE),
            "REPLY with nothing in flight: {stale:#x}"
        );
        let nobody = call(sys_op::REPLY, u64::MAX, reply.as_ptr() as u64, 0, 0);
        check!(nobody == failed(ESRCH), "REPLY to no provider: {nobody:#x}");
        Ok(())
    })();
    teardown();
    result
}

pub fn hostile_buffers() -> Result<(), String> {
    let id = sys_setup()?;
    let result = (|| {
        let root = Id::ROOT;
        // A payload that does not fit the daemon's buffer: refused, and the
        // request stays queued for the next NEXT.
        // The mount root's lookup has an empty payload: cache it first, so
        // the trick meets a request with a path.
        vfsapi::abi_stat(root, "/mnt/s").map_err(|e| format!("stat: {e:?}"))?;
        take_results();
        trick(Trick::SmallCap);
        let stat = vfsapi::abi_stat(root, "/mnt/s/zz");
        check!(
            stat == Err(FsError::NotFound),
            "lookup after a small buffer: {stat:?}"
        );
        let results = take_results();
        check!(
            results.first() == Some(&failed(EINVAL)) && results[1..] == [1, 0],
            "small buffer: {results:x?}"
        );
        // An unwritable record: nothing is taken.
        trick(Trick::BadRecord);
        vfsapi::abi_create(root, "/mnt/s/g", 0o644).map_err(|e| format!("create: {e:?}"))?;
        let results = take_results();
        check!(
            results.first() == Some(&failed(EFAULT)) && results[1..] == [1, 0],
            "bad record: {results:x?}"
        );
        vfsapi::abi_write(root, "/mnt/s/g", 0, b"abcd").map_err(|e| format!("write: {e:?}"))?;
        take_results();
        // An unreadable reply record answers nothing: the request times out,
        // the caller's buffer is untouched, and the daemon lives on.
        trick(Trick::BadReply);
        let mut buf = [0x99u8; 4];
        let got = vfsapi::abi_read_at(root, "/mnt/s/g", 0, &mut buf);
        check!(got == Err(FsError::Io), "an unreadable reply: {got:?}");
        check!(buf == [0x99; 4], "a failed read changed the buffer");
        let results = take_results();
        check!(
            results.get(..2) == Some(&[1, failed(EFAULT)][..]),
            "bad reply: {results:x?}"
        );
        let (stats, alive) = fuse::stats(id as usize).ok_or("no stats")?;
        check!(
            alive && stats.timeouts == 1,
            "after the bad reply: {stats:?}"
        );
        let got = vfsapi::abi_read_at(root, "/mnt/s/g", 0, &mut buf);
        check!(got == Ok(4) && &buf == b"abcd", "no recovery: {got:?}");
        Ok(())
    })();
    teardown();
    result
}

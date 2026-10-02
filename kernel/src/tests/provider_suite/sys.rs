//! Syscall 32: who may provide, and the REGISTER / NEXT / COMPLETE cycle
//! driven through the syscall surface with hostile records, lengths and
//! buffers.

use super::*;
use crate::block::provider::sys::{self as storage, op, FLAG_WRITABLE};
use crate::ipc::credentials::CAP_SYS_ADMIN;
use crate::user_ptr;

const EPERM: i64 = 1;
const ESRCH: i64 = 3;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const ESTALE: i64 = 116;

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn call(operation: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    storage::dispatch(operation, a1, a2, a3, a4)
}

fn register_call(sectors: u64, sector_size: u64, flags: u64) -> u64 {
    let info = [sectors, sector_size, flags, 0];
    // Through the gate's own test entry, like a real REGISTER.
    process::dispatch_for_test(32, op::REGISTER, info.as_ptr() as u64, 0)
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
    fresh_tasks();
    test_clock::recycle_all();
    crate::fs::late::reset_for_tests(None);
    let result = (|| {
        let usb = usbpolicy::USB_UID;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            register_call(64, 512, 1) == failed(EPERM),
            "the kernel task registered"
        );
        task_with(usb, 0)?;
        check!(
            register_call(64, 512, 1) == failed(EPERM),
            "_usb without the capability registered"
        );
        task_with(1000, CAP_BLOCK_PROVIDER)?;
        check!(
            register_call(64, 512, 1) == failed(EPERM),
            "another uid with the capability registered"
        );
        check!(
            call(op::REMOVE, 0, 0, 0, 0) == failed(EPERM),
            "another uid removed a disk"
        );
        check!(
            call(op::SCANNED, 0, 0, 0, 0) == failed(EPERM),
            "another uid reported a scan"
        );
        task_with(0, CAP_BLOCK_PROVIDER | CAP_SYS_ADMIN)?;
        check!(
            register_call(64, 512, 1) == failed(EPERM),
            "root registered"
        );

        task_with(usb, CAP_BLOCK_PROVIDER)?;
        check!(
            register_call(64, 4096, 1) == failed(EINVAL),
            "4 KiB sectors"
        );
        check!(register_call(64, 512, 4) == failed(EINVAL), "unknown flags");
        check!(register_call(0, 512, 0) == failed(EINVAL), "an empty disk");
        let previous = user_ptr::set_trust_kernel_pointers(false);
        let null = process::dispatch_for_test(32, op::REGISTER, 0, 0);
        user_ptr::set_trust_kernel_pointers(previous);
        check!(null == failed(EFAULT), "a null record: {null:#x}");
        check!(call(9, 0, 0, 0, 0) == failed(EINVAL), "an unknown op");
        check!(
            call(op::SCANNED, 0, 0, 0, 0) == 0,
            "the provider could not report its scan"
        );
        for id in 0..provider::MAX_PROVIDERS as u64 {
            let got = register_call(64 + id, 512, FLAG_WRITABLE);
            check!(got == id, "registration {id} returned {got:#x}");
        }
        check!(register_call(64, 512, 1) == failed(EBUSY), "a ninth disk");
        let device = crate::block::device("usb7").ok_or("usb7 not in the registry")?;
        check!(
            device.sector_count() == 71 && device.is_writable(),
            "usb7 geometry"
        );
        // A disk answers only the task that registered it.
        let me = task::current();
        task_with(usb, CAP_BLOCK_PROVIDER)?;
        let mut req = [0u64; 4];
        let next = call(op::NEXT, 0, req.as_mut_ptr() as u64, 0, 0);
        check!(
            next == failed(ESRCH),
            "another _usb task took a request: {next:#x}"
        );
        check!(
            call(op::REMOVE, 0, 0, 0, 0) == failed(ESRCH),
            "another _usb task removed a disk"
        );
        task::harness::switch_current(me);
        check!(
            call(op::REMOVE, 0, 0, 0, 0) == 0,
            "the owner could not remove its disk"
        );

        // SETTLE is init's: CAP_SYS_ADMIN, not the provider capability.
        test_clock::recycle_all();
        crate::fs::late::reset_for_tests(None);
        check!(
            call(op::SETTLE, 0, 0, 0, 0) == failed(EPERM),
            "the provider settled"
        );
        task_with(0, CAP_SYS_ADMIN)?;
        let settled = process::dispatch_for_test(32, op::SETTLE, 0, 0);
        check!(
            settled == u64::from(crate::fs::late::state::NONE),
            "settle with nothing pending: {settled:#x}"
        );
        Ok(())
    })();
    teardown();
    result
}

/// How the syscall-level fake provider misbehaves on its next turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Trick {
    None,
    /// NEXT with a data buffer too small for the write.
    SmallCap,
    /// NEXT with an unwritable request record.
    BadRecord,
    /// COMPLETE a read from an unreadable buffer.
    BadData,
}

static TRICK: Mutex<(Trick, Vec<u64>)> = Mutex::new((Trick::None, Vec::new()));

fn trick(trick: Trick) {
    TRICK.lock().0 = trick;
}

/// Return codes the fake provider saw, oldest first.
fn take_results() -> Vec<u64> {
    core::mem::take(&mut TRICK.lock().1)
}

/// The fake provider, through syscall 32: NEXT into its own buffers, serve
/// from the backing store, COMPLETE.
fn serve_sys(index: usize) {
    let trick = core::mem::replace(&mut TRICK.lock().0, Trick::None);
    let mut req = [0u64; 4];
    let mut data = vec![0u8; provider::MAX_REQUEST_BYTES];
    let cap = if trick == Trick::SmallCap {
        SECTOR_SIZE as u64
    } else {
        data.len() as u64
    };
    let record = req.as_mut_ptr() as u64;
    let got = if trick == Trick::BadRecord {
        let previous = user_ptr::set_trust_kernel_pointers(false);
        let got = call(op::NEXT, index as u64, 0x10, data.as_mut_ptr() as u64, cap);
        user_ptr::set_trust_kernel_pointers(previous);
        got
    } else {
        call(
            op::NEXT,
            index as u64,
            record,
            data.as_mut_ptr() as u64,
            cap,
        )
    };
    TRICK.lock().1.push(got);
    if got != 1 {
        test_clock::advance(provider::SLICE_TICKS);
        return;
    }
    let [tag, operation, lba, bytes] = req;
    let start = lba as usize * SECTOR_SIZE;
    let end = start + bytes as usize;
    let mut guard = FAKE.lock();
    let fake = guard.as_mut().expect("fake provider");
    fake.served += 1;
    match operation {
        1 => fake.data.read(start, &mut data[..end - start]),
        2 => fake.data.write(start, &data[..end - start]),
        _ => fake.flushes += 1,
    }
    let done = if trick == Trick::BadData {
        let previous = user_ptr::set_trust_kernel_pointers(false);
        let done = call(op::COMPLETE, index as u64, tag, status::OK, 0x10);
        user_ptr::set_trust_kernel_pointers(previous);
        done
    } else {
        call(
            op::COMPLETE,
            index as u64,
            tag,
            status::OK,
            data.as_ptr() as u64,
        )
    };
    TRICK.lock().1.push(done);
}

fn sys_setup() -> Result<&'static dyn BlockDevice, String> {
    let disk = setup(Mode::Normal)?;
    test_clock::set_server(Some(serve_sys));
    trick(Trick::None);
    take_results();
    Ok(disk)
}

pub fn request_cycle() -> Result<(), String> {
    let disk = sys_setup()?;
    let result = (|| {
        let mut out = vec![0u8; 3 * SECTOR_SIZE];
        pattern(9, 1, &mut out);
        disk.write_sectors(9, &out)
            .map_err(|e| format!("write: {e:?}"))?;
        let mut back = vec![0u8; out.len()];
        disk.read_sectors(9, &mut back)
            .map_err(|e| format!("read: {e:?}"))?;
        check!(back == out, "read back other bytes");
        disk.flush().map_err(|e| format!("flush: {e:?}"))?;
        check!(
            with_fake(|fake| fake.flushes) == 1,
            "the flush did not arrive"
        );
        let results = take_results();
        check!(results == [1, 0, 1, 0, 1, 0], "provider saw {results:x?}");
        // Nothing queued: NEXT reports none, COMPLETE is stale.
        let id = with_fake(|fake| fake.disk) as u64;
        let mut req = [0u64; 4];
        let next = call(op::NEXT, id, req.as_mut_ptr() as u64, 0, 0);
        check!(next == 0, "NEXT on an idle disk: {next:#x}");
        let stale = call(op::COMPLETE, id, 1, status::OK, 0);
        check!(
            stale == failed(ESTALE),
            "COMPLETE on an idle disk: {stale:#x}"
        );
        check!(
            call(op::COMPLETE, u64::MAX, 1, 0, 0) == failed(ESRCH),
            "COMPLETE on no disk"
        );
        check!(
            call(op::REMOVE, u64::MAX, 0, 0, 0) == failed(ESRCH),
            "REMOVE of no disk"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn hostile_lengths() -> Result<(), String> {
    let disk = sys_setup()?;
    let result = (|| {
        // A write's data does not fit the provider's buffer: refused, and the
        // request stays queued for the next NEXT.
        trick(Trick::SmallCap);
        let out = [0x3Cu8; 4 * SECTOR_SIZE];
        disk.write_sectors(20, &out)
            .map_err(|e| format!("write: {e:?}"))?;
        let results = take_results();
        check!(
            results == [failed(EINVAL), 1, 0],
            "small buffer: {results:x?}"
        );
        // An unwritable request record: nothing is taken.
        trick(Trick::BadRecord);
        let mut back = [0u8; 4 * SECTOR_SIZE];
        disk.read_sectors(20, &mut back)
            .map_err(|e| format!("read: {e:?}"))?;
        check!(back == out, "read back other bytes");
        let results = take_results();
        check!(
            results == [failed(EFAULT), 1, 0],
            "bad record: {results:x?}"
        );
        // Read data the kernel cannot copy: the read fails, the caller's
        // buffer is untouched, the disk lives on.
        trick(Trick::BadData);
        let mut sector = [0x99u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(20, &mut sector),
            BlockError::Io,
            "unreadable read data",
        )?;
        check!(
            sector.iter().all(|&b| b == 0x99),
            "a failed read changed the buffer"
        );
        let results = take_results();
        check!(results == [1, failed(EFAULT)], "bad data: {results:x?}");
        let (stats, alive) = provider::stats(with_fake(|fake| fake.disk)).ok_or("no stats")?;
        check!(
            alive && stats.timeouts == 0,
            "stats {stats:?} alive {alive}"
        );
        check!(
            disk.read_sectors(20, &mut sector).is_ok(),
            "the disk did not recover"
        );
        // A completion naming nothing in flight is stale.
        let id = with_fake(|fake| fake.disk) as u64;
        let stale = call(op::COMPLETE, id, 0, status::OK, 0);
        check!(
            stale == failed(ESTALE),
            "a completion with nothing in flight: {stale:#x}"
        );
        Ok(())
    })();
    teardown();
    result
}

//! User-space block providers (`block::provider`,
//! docs/architecture/usb-storage.md): the request path with a fake provider
//! that the waiting requester runs in place of parking, an ext2 volume and
//! the late `/home` mount on top of it, hostile replies, a provider that
//! dies or stops answering mid-request, and the syscall 33 gate.

mod hostile;
mod io;
mod sys;

use super::*;
use crate::block::provider::{self, status, test_clock, Op};
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::ipc::credentials::{self, Cred, CAP_BLOCK_PROVIDER};
use spin::Mutex;

/// Sectors of the fake stick (2 MiB: the late mount's partition must hold
/// the formatter's 1 MiB minimum).
const SECTORS: u64 = 4096;

/// How the fake provider answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Serve every request from the backing store.
    Normal,
    /// Take requests and complete them with this status code.
    Status(u64),
    /// Take a request and never answer (the clock moves on).
    Silent,
    /// Complete with a tag that is not the request's.
    WrongTag,
    /// Take the request, then die (`teardown_task`).
    DieAfterTake,
    /// Fail every `n`th request with an I/O error, serve the others.
    FlakyEvery(u64),
    /// Take the request, `SIGKILL` the requester (this slot) while it holds
    /// the request slot parked in the kernel, then serve it normally.
    KillRequester(usize),
}

/// A disk's bytes in 64 KiB chunks, each allocated when first written
/// (unwritten ones read as zeros). The suite runs late in the full kernel
/// run, on a heap the earlier suites left fragmented and well used: a 2 MiB
/// disk held in one piece could not be allocated there.
struct Store {
    chunks: Vec<Option<Vec<u8>>>,
}

const CHUNK: usize = 64 * 1024;

impl Store {
    fn new(len: usize) -> Store {
        Store {
            chunks: (0..len.div_ceil(CHUNK)).map(|_| None).collect(),
        }
    }

    /// The pieces of `start..start + len`: (chunk, offset in it, length).
    fn pieces(start: usize, len: usize) -> impl Iterator<Item = (usize, usize, usize)> {
        let mut at = start;
        let end = start + len;
        core::iter::from_fn(move || {
            if at >= end {
                return None;
            }
            let (chunk, offset) = (at / CHUNK, at % CHUNK);
            let take = (CHUNK - offset).min(end - at);
            at += take;
            Some((chunk, offset, take))
        })
    }

    fn read(&self, start: usize, out: &mut [u8]) {
        let mut done = 0;
        for (chunk, offset, take) in Self::pieces(start, out.len()) {
            match &self.chunks[chunk] {
                Some(bytes) => {
                    out[done..done + take].copy_from_slice(&bytes[offset..offset + take])
                }
                None => out[done..done + take].fill(0),
            }
            done += take;
        }
    }

    fn write(&mut self, start: usize, data: &[u8]) {
        let mut done = 0;
        for (chunk, offset, take) in Self::pieces(start, data.len()) {
            let bytes = self.chunks[chunk].get_or_insert_with(|| vec![0u8; CHUNK]);
            bytes[offset..offset + take].copy_from_slice(&data[done..done + take]);
            done += take;
        }
    }

    fn matches(&self, start: usize, data: &[u8]) -> bool {
        let mut done = 0;
        Self::pieces(start, data.len()).all(|(chunk, offset, take)| {
            let theirs = &data[done..done + take];
            done += take;
            match &self.chunks[chunk] {
                Some(bytes) => bytes[offset..offset + take] == *theirs,
                None => theirs.iter().all(|&byte| byte == 0),
            }
        })
    }
}

struct Fake {
    disk: usize,
    owner: usize,
    mode: Mode,
    data: Store,
    served: u64,
    flushes: u64,
    last: Option<provider::Request>,
    /// What [`Mode::KillRequester`] saw when it killed the requester.
    killed: Option<Result<(), String>>,
}

static FAKE: Mutex<Option<Fake>> = Mutex::new(None);

/// A provider task (`_usb` with `CAP_BLOCK_PROVIDER`), made current.
fn provider_task() -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    credentials::set(
        slot,
        Cred::new(
            usbpolicy::USB_UID,
            usbpolicy::USB_UID,
            CAP_BLOCK_PROVIDER,
            0,
            0,
        ),
    );
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Fresh slots, a provider task and a registered fake disk served in `mode`.
fn setup(mode: Mode) -> Result<&'static dyn BlockDevice, String> {
    fresh_tasks();
    test_clock::recycle_all();
    let owner = provider_task()?;
    let disk = provider::register(owner, SECTORS, true).map_err(|e| format!("register: {e:?}"))?;
    *FAKE.lock() = Some(Fake {
        disk,
        owner,
        mode,
        data: Store::new(SECTORS as usize * SECTOR_SIZE),
        served: 0,
        flushes: 0,
        last: None,
        killed: None,
    });
    test_clock::set_server(Some(serve));
    crate::block::device(&alloc::format!("usb{disk}"))
        .ok_or_else(|| String::from("not in the registry"))
}

/// A clean task table with only the kernel task, current.
fn fresh_tasks() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
}

/// Undo [`setup`].
fn teardown() {
    test_clock::recycle_all();
    *FAKE.lock() = None;
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
}

fn mode(mode: Mode) {
    if let Some(fake) = FAKE.lock().as_mut() {
        fake.mode = mode;
    }
}

/// The fake provider: take the queued request (if any) and answer it.
fn serve(index: usize) {
    let mut guard = FAKE.lock();
    let Some(fake) = guard.as_mut() else {
        return;
    };
    if fake.disk != index {
        return;
    }
    let mut written = Vec::new();
    let taken = provider::next(index, fake.owner, 0, &mut |bytes| {
        written.extend_from_slice(bytes);
        Ok(())
    });
    let Ok(Some(request)) = taken else {
        // Nothing to take: a silent provider lets time pass.
        test_clock::advance(provider::SLICE_TICKS);
        return;
    };
    fake.last = Some(request);
    fake.served += 1;
    let code = match fake.mode {
        Mode::Silent => {
            test_clock::advance(provider::SLICE_TICKS);
            return;
        }
        Mode::DieAfterTake => {
            provider::teardown_task(fake.owner);
            return;
        }
        Mode::WrongTag => {
            let result =
                provider::complete(index, fake.owner, request.tag ^ 1, status::OK, &mut |_| {
                    Ok(())
                });
            assert!(result.is_err(), "a wrong tag was accepted");
            test_clock::advance(provider::SLICE_TICKS);
            return;
        }
        Mode::KillRequester(requester) => {
            fake.killed = Some(kill_parked_requester(requester));
            fake.mode = Mode::Normal;
            status::OK
        }
        Mode::Status(code) => code,
        Mode::FlakyEvery(n) if fake.served % n == 0 => status::IO,
        Mode::Normal | Mode::FlakyEvery(_) => status::OK,
    };
    let start = request.lba as usize * SECTOR_SIZE;
    if code == status::OK {
        match request.op {
            Op::Write => fake.data.write(start, &written),
            Op::Flush => fake.flushes += 1,
            Op::Read => {}
        }
    }
    let data = &fake.data;
    let result = provider::complete(index, fake.owner, request.tag, code, &mut |bounce| {
        data.read(start, bounce);
        Ok(())
    });
    assert!(result.is_ok(), "completion refused: {result:?}");
}

/// `SIGKILL` `requester` from the kernel task while it waits, parked, for
/// the request it holds the slot for. It must be woken to unwind, not ended
/// in place: ended, it would never run again to release the slot.
fn kill_parked_requester(requester: usize) -> Result<(), String> {
    let queue = task::wait::WaitQueue::new(task::WaitKind::Block);
    queue.park(requester, None);
    task::harness::switch_current(task::KERNEL_TASK);
    let sent = crate::task::signal::send_to_slot(
        task::KERNEL_TASK,
        requester,
        crate::task::signal::SIGKILL,
        crate::task::signal::SigInfo::kernel(),
    );
    let state = task::harness::state(requester);
    let reason = task::harness::take_wake_reason(requester);
    task::harness::switch_current(requester);
    check!(sent.is_ok(), "SIGKILL refused: {sent:?}");
    check!(
        state == Some(task::TaskState::Runnable),
        "SIGKILL left the requester {state:?} mid-request"
    );
    check!(
        reason == Some(task::WakeReason::Interrupted),
        "the requester was woken with {reason:?}"
    );
    Ok(())
}

fn with_fake<T>(f: impl FnOnce(&mut Fake) -> T) -> T {
    f(FAKE.lock().as_mut().expect("fake provider"))
}

/// A deterministic pattern for sector `lba`.
fn pattern(lba: u64, seed: u8, buf: &mut [u8]) {
    for (index, byte) in buf.iter_mut().enumerate() {
        *byte = (lba as u8).wrapping_mul(31) ^ (index as u8).wrapping_add(seed);
    }
}

fn expect_err(result: Result<(), BlockError>, want: BlockError, what: &str) -> Result<(), String> {
    match result {
        Err(error) if error == want => Ok(()),
        other => Err(format!("{what}: {other:?}, wanted {want:?}")),
    }
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("provider_read_write_roundtrip", io::read_write_roundtrip),
    ("provider_large_requests_split", io::large_requests_split),
    (
        "provider_flush_reaches_the_driver",
        io::flush_reaches_the_driver,
    ),
    ("provider_ext2_on_a_stick", io::ext2_on_a_stick),
    ("provider_late_home_mount", io::late_home_mount),
    ("provider_stress", io::stress),
    ("provider_error_statuses", hostile::error_statuses),
    ("provider_wrong_tag_times_out", hostile::wrong_tag_times_out),
    ("provider_silent_driver_dies", hostile::silent_driver_dies),
    ("provider_death_mid_request", hostile::death_mid_request),
    ("provider_dead_owner_detected", hostile::dead_owner_detected),
    ("provider_medium_gone", hostile::medium_gone),
    ("provider_stale_completion", hostile::stale_completion),
    (
        "provider_kill_mid_request_releases_slot",
        hostile::kill_mid_request_releases_slot,
    ),
    ("provider_sys_gate", sys::gate),
    ("provider_sys_request_cycle", sys::request_cycle),
    ("provider_sys_hostile_lengths", sys::hostile_lengths),
];

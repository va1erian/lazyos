//! User-space block providers: a ring-3 driver (`usbd`, for a USB stick)
//! serves a block device to the kernel (docs/architecture/usb-storage.md).
//!
//! Block drivers live in the kernel and USB lives in user space
//! (docs/driver-plan.md D1/D2), so a stick reaches the filesystem through
//! this seam. A provider registers a [`UserDisk`] (syscall 33, [`sys`]); the
//! disk joins the block registry under `usb<n>` and filesystems use it like
//! any other [`BlockDevice`].
//!
//! # One request at a time, through a kernel bounce buffer
//!
//! A filesystem call becomes a request in the disk's single request slot: a
//! write's data is copied into the disk's kernel-owned bounce buffer first.
//! The provider's `NEXT` takes the request (and a copy of the write data);
//! its `COMPLETE` names the request by tag, and a read's data is copied from
//! the provider into the bounce buffer only when its length is exactly the
//! request's. The waiting task copies the bounce buffer into its own buffer
//! after checking the tag is still its own. The provider never sees a kernel
//! address and never writes kernel memory but the bounce buffer.
//!
//! # Waiting
//!
//! The requester parks on the disk's `done` queue in slices of
//! [`SLICE_TICKS`], checking at each wake that the provider task is still
//! alive, until the request completes or its deadline passes. The deadline
//! has two parts: the provider must take a queued request within
//! [`QUEUE_TICKS`] (a provider that does not even look is stuck), then finish
//! it within [`TAKEN_TICKS`] (generous: a real USB stick can stall a write
//! for seconds while its flash reorganises, and the driver retries on top;
//! issue #704). A queued request whose provider is busy with another of its
//! disks (it serves them one transfer at a time) is not neglected: its queue
//! deadline restarts. A timed-out request is abandoned (a late completion is
//! refused as stale); after
//! [`DEAD_AFTER_TIMEOUTS`] timeouts in a row, or when the provider dies or
//! reports the medium gone, the disk is dead and every request, pending and
//! future, fails at once with [`BlockError::Io`]. A mount on a dead disk
//! stays mounted and fails its I/O (it degrades; nothing hangs).
//!
//! The requester holds the filesystem's locks while it waits; those are
//! `task::relax::YieldMutex`es, so other tasks give the CPU away instead of
//! spinning. A context that holds the task table cannot park at all and its
//! request fails ([`crate::task::relax::can_block`]).

mod request;
pub mod sys;
pub mod test_clock;

use alloc::vec::Vec;
use spin::Mutex;

use super::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::task::{self, wait::WaitQueue, WaitKind, WakeReason};

/// How many providers can register per boot (a dead disk's slot is never
/// reused: mounts may still hold it).
pub const MAX_PROVIDERS: usize = 8;
/// The largest single request, and the size of each disk's bounce buffer.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// How long a queued request may wait for the provider to take it (PIT
/// ticks, 100 Hz): 10 s. The provider polls at least once a second.
pub const QUEUE_TICKS: u64 = 1000;
/// How long the provider may work on a request it took: 60 s. `usbd` gives
/// up on a request after 45 s of its own (Linux allows a SCSI command 30 s).
pub const TAKEN_TICKS: u64 = 6000;
/// The longest a requester waits for the request slot: a holder's whole
/// time, which includes waiting up to a [`TAKEN_TICKS`] behind its
/// provider's work on another disk (`busy_elsewhere`), then its own queued
/// and taken time.
pub const SLOT_TICKS: u64 = QUEUE_TICKS + 2 * TAKEN_TICKS;
/// How often a waiting requester checks that its provider is alive.
pub const SLICE_TICKS: u64 = 10;
/// Consecutive timeouts after which a disk is declared dead.
pub const DEAD_AFTER_TIMEOUTS: u32 = 2;

const NAMES: [&str; MAX_PROVIDERS] = [
    "usb0", "usb1", "usb2", "usb3", "usb4", "usb5", "usb6", "usb7",
];

/// A request's operation, as the provider sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read = 1,
    Write = 2,
    Flush = 3,
}

/// Completion status codes a provider reports.
pub mod status {
    pub const OK: u64 = 0;
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // any other code means the same
    pub const IO: u64 = 1;
    pub const READ_ONLY: u64 = 2;
    /// The medium is gone: the disk dies with this request.
    pub const GONE: u64 = 3;
}

/// One request, as handed to the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub tag: u64,
    pub op: Op,
    pub lba: u64,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Taken,
    Done(Result<(), BlockError>),
}

/// Why a provider call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// No free slot, or the block registry is full.
    Full,
    /// Not a disk this task registered, or the disk is dead.
    NotOwner,
    /// Geometry or length out of range.
    Invalid,
    /// The completion names no request in flight (late, or forged).
    Stale,
    /// Copying to or from the provider's memory failed.
    Fault,
}

/// Counters, for the log and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub requests: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub stale: u64,
}

struct State {
    registered: bool,
    alive: bool,
    owner: usize,
    sectors: u64,
    writable: bool,
    /// A requester owns the request slot.
    busy: bool,
    phase: Phase,
    request: Request,
    /// When the provider took the request in flight (`now()`).
    taken_at: u64,
    bounce: Vec<u8>,
    next_tag: u64,
    timeouts: u32,
    /// Whether the late-mount path has scanned this disk's partitions.
    scanned: bool,
    stats: Stats,
}

/// A block device served by a user-space provider.
pub struct UserDisk {
    index: usize,
    state: Mutex<State>,
    /// The provider, waiting for a request.
    work: WaitQueue,
    /// The requester, waiting for its completion.
    done: WaitQueue,
    /// Requesters waiting for the request slot.
    idle: WaitQueue,
}

impl UserDisk {
    const fn new(index: usize) -> UserDisk {
        UserDisk {
            index,
            state: Mutex::new(State {
                registered: false,
                alive: false,
                owner: 0,
                sectors: 0,
                writable: false,
                busy: false,
                phase: Phase::Idle,
                request: Request {
                    tag: 0,
                    op: Op::Read,
                    lba: 0,
                    bytes: 0,
                },
                taken_at: 0,
                bounce: Vec::new(),
                next_tag: 0,
                timeouts: 0,
                scanned: false,
                stats: Stats {
                    requests: 0,
                    errors: 0,
                    timeouts: 0,
                    stale: 0,
                },
            }),
            work: WaitQueue::new(WaitKind::Block),
            done: WaitQueue::new(WaitKind::Block),
            idle: WaitQueue::new(WaitKind::Block),
        }
    }
}

static DISKS: [UserDisk; MAX_PROVIDERS] = [
    UserDisk::new(0),
    UserDisk::new(1),
    UserDisk::new(2),
    UserDisk::new(3),
    UserDisk::new(4),
    UserDisk::new(5),
    UserDisk::new(6),
    UserDisk::new(7),
];

/// The disk with registry index `id`, if it was ever registered.
fn disk(id: usize) -> Option<&'static UserDisk> {
    DISKS.get(id).filter(|disk| disk.state.lock().registered)
}

/// The clock requests are timed by (a test can move it forward).
fn now() -> u64 {
    task::ticks() + test_clock::offset()
}

/// Register a disk of `sectors` 512-byte sectors served by task `owner`.
/// Returns its id (the `n` of `usb<n>`).
pub fn register(owner: usize, sectors: u64, writable: bool) -> Result<usize, ProviderError> {
    if sectors == 0 || sectors.checked_mul(SECTOR_SIZE as u64).is_none() {
        return Err(ProviderError::Invalid);
    }
    let disk = DISKS
        .iter()
        .find(|disk| {
            let mut state = disk.state.lock();
            if state.registered {
                return false;
            }
            // Claim the slot before the lock drops, so two registrations
            // cannot take the same one.
            state.registered = true;
            true
        })
        .ok_or(ProviderError::Full)?;
    {
        let mut state = disk.state.lock();
        state.bounce.resize(MAX_REQUEST_BYTES, 0);
        state.owner = owner;
        state.sectors = sectors;
        state.writable = writable;
        state.alive = true;
        state.scanned = false;
        // Tags carry the slot, so one disk's tag never matches another's.
        state.next_tag = (disk.index as u64) << 56;
    }
    // The registry never forgets a device; a slot the test suite recycles
    // is already there under its name.
    let known = super::device(NAMES[disk.index]).is_some();
    if !known && super::register(disk).is_err() {
        disk.state.lock().alive = false;
        return Err(ProviderError::Full);
    }
    serial_println!(
        "block: {} registered by task {owner}: {sectors} sectors ({})",
        NAMES[disk.index],
        if writable { "rw" } else { "ro" }
    );
    Ok(disk.index)
}

/// Take the next request of disk `id` for its provider `owner`, waiting
/// until `deadline` (absolute ticks) for one. A write's data is handed to
/// `copy_out` first; if that fails the request stays queued.
pub fn next(
    id: usize,
    owner: usize,
    deadline: u64,
    copy_out: &mut dyn FnMut(&[u8]) -> Result<(), ProviderError>,
) -> Result<Option<Request>, ProviderError> {
    let disk = disk(id).ok_or(ProviderError::NotOwner)?;
    loop {
        {
            let mut state = disk.state.lock();
            if state.owner != owner || !state.alive {
                return Err(ProviderError::NotOwner);
            }
            if state.phase == Phase::Queued {
                let request = state.request;
                if request.op == Op::Write {
                    copy_out(&state.bounce[..request.bytes])?;
                }
                state.phase = Phase::Taken;
                state.taken_at = now();
                return Ok(Some(request));
            }
        }
        // A killed provider stops waiting for work (nothing of its is in
        // flight here) so it reaches its syscall return and dies.
        if now() >= deadline
            || !task::relax::can_block()
            || test_clock::serving()
            || task::signal::killed(task::current())
        {
            return Ok(None);
        }
        park(&disk.work, deadline);
    }
}

/// Complete request `tag` of disk `id` with `code` ([`status`]). A
/// successful read must hand exactly the request's bytes to `copy_in`.
pub fn complete(
    id: usize,
    owner: usize,
    tag: u64,
    code: u64,
    copy_in: &mut dyn FnMut(&mut [u8]) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let disk = disk(id).ok_or(ProviderError::NotOwner)?;
    let mut state = disk.state.lock();
    if state.owner != owner || !state.alive {
        return Err(ProviderError::NotOwner);
    }
    if state.phase != Phase::Taken || state.request.tag != tag {
        state.stats.stale += 1;
        return Err(ProviderError::Stale);
    }
    let request = state.request;
    let mut refused = None;
    let result = match code {
        status::OK if request.op == Op::Read => {
            let bytes = request.bytes;
            match copy_in(&mut state.bounce[..bytes]) {
                Ok(()) => Ok(()),
                Err(error) => {
                    refused = Some(error);
                    Err(BlockError::Io)
                }
            }
        }
        status::OK => Ok(()),
        status::READ_ONLY => Err(BlockError::ReadOnly),
        status::GONE => {
            state.alive = false;
            serial_println!("block: {}: provider reports the medium gone", NAMES[id]);
            Err(BlockError::Io)
        }
        _ => Err(BlockError::Io),
    };
    state.phase = Phase::Done(result);
    let alive = state.alive;
    drop(state);
    disk.done.notify_all();
    if !alive {
        wake_all(disk);
    }
    refused.map_or(Ok(()), Err)
}

/// The provider says its device is gone: the disk dies.
pub fn remove(id: usize, owner: usize) -> Result<(), ProviderError> {
    let disk = disk(id).ok_or(ProviderError::NotOwner)?;
    {
        let mut state = disk.state.lock();
        if state.owner != owner || !state.alive {
            return Err(ProviderError::NotOwner);
        }
        state.alive = false;
    }
    serial_println!("block: {} removed by its provider", NAMES[id]);
    wake_all(disk);
    Ok(())
}

/// Task `slot` is gone: every disk it served dies (`ipc::teardown_task`).
pub fn teardown_task(slot: usize) {
    for disk in DISKS.iter() {
        let died = {
            let mut state = disk.state.lock();
            let died = state.registered && state.alive && state.owner == slot;
            if died {
                state.alive = false;
            }
            died
        };
        if died {
            serial_println!("block: {}: provider task {slot} died", NAMES[disk.index]);
            wake_all(disk);
        }
    }
}

fn wake_all(disk: &UserDisk) {
    disk.work.notify_all();
    disk.done.notify_all();
    disk.idle.notify_all();
}

/// Live disks the late-mount path has not scanned yet; each is returned once.
pub fn take_unscanned() -> Vec<&'static dyn BlockDevice> {
    DISKS
        .iter()
        .filter(|disk| {
            let mut state = disk.state.lock();
            let fresh = state.registered && state.alive && !state.scanned;
            state.scanned |= fresh;
            fresh
        })
        .map(|disk| disk as &'static dyn BlockDevice)
        .collect()
}

/// Whether `name` is a provider disk or one of its partitions.
pub fn is_provider_device(name: &str) -> bool {
    NAMES.iter().any(|disk| {
        name.strip_prefix(disk)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('p'))
    })
}

/// Whether task `slot` serves a live disk: such a task must never wait for
/// the VFS, which a requester may hold while it waits for that task.
pub fn serves_disk(slot: usize) -> bool {
    DISKS.iter().any(|disk| {
        let state = disk.state.lock();
        state.registered && state.alive && state.owner == slot
    })
}

/// Whether `owner` is working, within its deadline, on a request of a disk
/// other than `index`. A request queued behind it then waits its turn
/// without counting as neglected; a request past its own deadline is not
/// work, so the wait stays bounded.
fn busy_elsewhere(index: usize, owner: usize, now: u64) -> bool {
    DISKS.iter().filter(|disk| disk.index != index).any(|disk| {
        let state = disk.state.lock();
        state.registered
            && state.alive
            && state.owner == owner
            && state.phase == Phase::Taken
            && now < state.taken_at + TAKEN_TICKS
    })
}

/// Disk `id`'s counters and whether it is alive.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // tests and diagnostics
pub fn stats(id: usize) -> Option<(Stats, bool)> {
    let state = DISKS.get(id)?.state.lock();
    state.registered.then_some((state.stats, state.alive))
}

/// Park the current task on `queue` until notified or `deadline`, with
/// interrupts off as [`WaitQueue::wait`] requires. A signal does not end the
/// wait; one tick is let through so the provider can run. A killed requester
/// (whose every wait now returns at once) keeps waiting in such one-tick
/// naps until its request completes or its deadline passes: an in-flight
/// request is never abandoned, so the request slot and the filesystem state
/// around it stay consistent, and the release path always runs.
fn park(queue: &WaitQueue, deadline: u64) {
    let enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let ticks = deadline.saturating_sub(test_clock::offset());
    if queue.wait(task::current(), Some(ticks)) == WakeReason::Interrupted {
        task::nap();
    }
    if enabled {
        x86_64::instructions::interrupts::enable();
    }
}

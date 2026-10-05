//! User-space filesystems: a ring-3 daemon serves a directory tree at
//! `/mnt/<name>` (docs/smb-plan.md §3, stage F1).
//!
//! The kernel side is a translator, not a filesystem: [`backend::FuseFs`]
//! implements [`Filesystem`] by turning every call into a request in the
//! provider's single request slot, and the daemon takes it (syscall 35
//! `NEXT`, [`sys`]) and answers it (`REPLY`). The protocol, both records and
//! every payload, is `libs/fused`, which the daemons link too.
//!
//! # One request at a time, through a kernel bounce buffer
//!
//! This is the user-space block provider's design (`block/provider.rs`)
//! applied to files. A request's path and data are copied into the slot's
//! kernel-owned bounce buffer; `NEXT` copies them out to the daemon; `REPLY`
//! names the request by tag and copies the answer's data into the bounce
//! buffer, refused when it is longer than the request allowed. The waiting
//! task copies it out after checking the request is still its own. The
//! daemon never sees a kernel address. Each request carries at most
//! [`fused::wire::MAX_DATA`] bytes: the backend splits longer reads and
//! writes.
//!
//! # Waiting
//!
//! The requester parks in slices of [`SLICE_TICKS`], checking the daemon is
//! alive, until the reply comes or [`REQUEST_TICKS`] pass. After
//! [`DEAD_AFTER_TIMEOUTS`] timeouts in a row, or when the daemon exits,
//! unregisters or dies, the provider is dead: every request, pending and
//! future, fails at once with [`FsError::Io`].
//!
//! # The mount table lock
//!
//! The VFS holds its mount table (a `YieldMutex`) across a filesystem call,
//! so while a path operation (lookup, create, readdir, ...) waits here, every
//! other VFS caller waits too, and a daemon that touched the VFS while
//! serving would wait for itself until the deadline. Open files are read and
//! written by node ([`Filesystem::open_node`]) without that lock, so the bulk
//! of the traffic does not hold it. A daemon must not use the filesystem on
//! its request path.
//!
//! # Mounting and unmounting
//!
//! `REGISTER` mounts the new provider at `/mnt/<name>` in both mount tables
//! (native and Linux ABI), always `nosuid`. `UNREGISTER` unmounts at once. A
//! daemon that dies cannot be unmounted from its teardown (which may run
//! where the mount table cannot be waited for), so its mount fails every call
//! until [`reap`] (from the periodic flusher, never waiting) or the next
//! `REGISTER` of the same name removes it.

mod backend;
mod channel;
mod mount;
pub mod sys;
pub mod test_hook;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use fused::wire::{Reply, Request};

use super::vfs::FsError;
use crate::task::{self, wait::WaitQueue, WaitKind, WakeReason};

pub use backend::FuseFs;
#[cfg_attr(not(lazyos_tests), allow(unused_imports))] // the suite's bound check
pub use backend::MAX_DIR_ENTRIES;
pub use mount::{reap, register, teardown_task, unregister};

/// How many daemons can be mounted at once.
pub const MAX_PROVIDERS: usize = 8;
/// How long one request may take (PIT ticks, 100 Hz): 10 s.
pub const REQUEST_TICKS: u64 = 1000;
/// How often a waiting requester checks that its daemon is alive.
pub const SLICE_TICKS: u64 = 10;
/// Consecutive timeouts after which a provider is declared dead.
pub const DEAD_AFTER_TIMEOUTS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Taken,
    /// A well-formed reply arrived (its status is the backend's to read), or
    /// the request failed.
    Done(Result<(), FsError>),
}

/// Why a provider call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuseError {
    /// Every slot is in use.
    Full,
    /// A live provider already serves that name.
    Busy,
    /// `/mnt` is not a directory on this system.
    NoMountRoot,
    /// Not a provider this task registered, or it is dead.
    NotOwner,
    /// A name, flag or length out of range.
    Invalid,
    /// The reply names no request in flight (late, or forged).
    Stale,
    /// Copying to or from the daemon's memory failed.
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
    /// The slot is taken: by a live provider, or a dead one still mounted.
    registered: bool,
    alive: bool,
    /// Still in the mount tables.
    mounted: bool,
    owner: usize,
    /// Bumped at every registration, so a mount or node of an earlier
    /// provider in this slot can never reach a later one.
    epoch: u64,
    name: String,
    /// A requester owns the request slot.
    busy: bool,
    phase: Phase,
    request: Request,
    /// Bytes of the bounce buffer the request's payload fills.
    payload_len: usize,
    /// The most reply data the request accepts.
    reply_cap: usize,
    reply: Reply,
    bounce: Vec<u8>,
    next_tag: u64,
    timeouts: u32,
    stats: Stats,
}

/// One provider slot.
struct Slot {
    index: usize,
    state: Mutex<State>,
    /// The daemon, waiting for a request.
    work: WaitQueue,
    /// The requester, waiting for its reply.
    done: WaitQueue,
    /// Requesters waiting for the request slot.
    idle: WaitQueue,
}

impl Slot {
    const fn new(index: usize) -> Slot {
        Slot {
            index,
            state: Mutex::new(State {
                registered: false,
                alive: false,
                mounted: false,
                owner: 0,
                epoch: 0,
                name: String::new(),
                busy: false,
                phase: Phase::Idle,
                request: Request::EMPTY,
                payload_len: 0,
                reply_cap: 0,
                reply: Reply::EMPTY,
                bounce: Vec::new(),
                next_tag: 0,
                timeouts: 0,
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

    fn wake_all(&self) {
        self.work.notify_all();
        self.done.notify_all();
        self.idle.notify_all();
    }
}

static SLOTS: [Slot; MAX_PROVIDERS] = [
    Slot::new(0),
    Slot::new(1),
    Slot::new(2),
    Slot::new(3),
    Slot::new(4),
    Slot::new(5),
    Slot::new(6),
    Slot::new(7),
];

/// Set when a provider died still mounted: [`reap`] has work.
static REAP: AtomicBool = AtomicBool::new(false);

/// The clock requests are timed by (a test can move it forward).
fn now() -> u64 {
    task::ticks() + test_hook::offset()
}

/// Take the next request of provider `id` for its daemon `owner`, waiting
/// until `deadline` (absolute ticks) for one. Its payload is handed to
/// `copy_out` first; if that fails the request stays queued.
pub fn next(
    id: usize,
    owner: usize,
    deadline: u64,
    copy_out: &mut dyn FnMut(&[u8]) -> Result<(), FuseError>,
) -> Result<Option<Request>, FuseError> {
    let slot = SLOTS.get(id).ok_or(FuseError::NotOwner)?;
    loop {
        {
            let mut state = slot.state.lock();
            if !state.registered || state.owner != owner || !state.alive {
                return Err(FuseError::NotOwner);
            }
            if state.phase == Phase::Queued {
                let request = state.request;
                copy_out(&state.bounce[..state.payload_len])?;
                state.phase = Phase::Taken;
                return Ok(Some(request));
            }
        }
        if now() >= deadline
            || !task::relax::can_block()
            || test_hook::serving()
            || task::signal::killed(task::current())
        {
            return Ok(None);
        }
        park(&slot.work, deadline);
    }
}

/// Answer the request `reply.tag` of provider `id`. A reply carrying data
/// hands exactly `reply.data_len` bytes to `copy_in`; more than the request
/// accepts is refused and fails the request.
pub fn reply(
    id: usize,
    owner: usize,
    reply: &Reply,
    copy_in: &mut dyn FnMut(&mut [u8]) -> Result<(), FuseError>,
) -> Result<(), FuseError> {
    let slot = SLOTS.get(id).ok_or(FuseError::NotOwner)?;
    let mut state = slot.state.lock();
    if !state.registered || state.owner != owner || !state.alive {
        return Err(FuseError::NotOwner);
    }
    if state.phase != Phase::Taken || state.request.tag != reply.tag {
        state.stats.stale += 1;
        return Err(FuseError::Stale);
    }
    let len = usize::try_from(reply.data_len).unwrap_or(usize::MAX);
    let (result, refused) = if len > state.reply_cap {
        (Err(FsError::Io), Some(FuseError::Invalid))
    } else {
        match copy_in(&mut state.bounce[..len]) {
            Ok(()) => (Ok(()), None),
            Err(error) => (Err(FsError::Io), Some(error)),
        }
    };
    state.reply = *reply;
    state.phase = Phase::Done(result);
    drop(state);
    slot.done.notify_all();
    refused.map_or(Ok(()), Err)
}

/// Provider `id`'s counters and whether it is alive.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // tests and diagnostics
pub fn stats(id: usize) -> Option<(Stats, bool)> {
    let state = SLOTS.get(id)?.state.lock();
    state.registered.then_some((state.stats, state.alive))
}

/// Park the current task on `queue` until notified or `deadline`, with
/// interrupts off as [`WaitQueue::wait`] requires. A signal does not end the
/// wait (the request in flight is never abandoned, so the slot stays
/// consistent); one tick is let through so the daemon can run.
fn park(queue: &WaitQueue, deadline: u64) {
    let enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let ticks = deadline.saturating_sub(test_hook::offset());
    if queue.wait(task::current(), Some(ticks)) == WakeReason::Interrupted {
        task::nap();
    }
    if enabled {
        x86_64::instructions::interrupts::enable();
    }
}

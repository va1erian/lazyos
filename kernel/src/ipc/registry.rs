//! Messenger name registry (issue #89).
//!
//! This is the kernel half of `docs/messenger.md` sections 8 and 13: a bounded
//! table of `Register(name, endpoint, interfaces)` entries with an owner, an
//! optional lease, and a `Resolve(name)` path that hands the caller a fresh
//! handle to the registered endpoint. Looking a name up is a capability
//! transfer, never a pointer copy: the resolved operation opens the endpoint in
//! the *target task's* handle table (the caller, or a client the privileged
//! `messengerd` proxy names), so handle numbers never leak across tasks.
//!
//! The table is the authority, but services are free to publish through either
//! path: any task may call the native `register` op directly (the owner is the
//! caller), or send the request to `messengerd` over the bootstrap channel,
//! which forwards it with the caller as owner. Both end in [`register`].
//!
//! # Leases and owner death
//!
//! Every entry carries an owner slot and an optional lease deadline:
//!
//! * `release_owner` drops every name a slot owns. It is the teardown hook for
//!   process death: the registry cannot be wired into `reap_child` directly
//!   while `task/**` is frozen for this issue, so the hook is public, called by
//!   the tests today, and ready for the task teardown loop to call later.
//! * Access-time pruning (every register/resolve/list/unregister) also removes
//!   entries whose owner slot no longer holds a live task and entries whose
//!   deadline passed. A crashed owner that never runs teardown cannot leave a
//!   name that resolves forever.
//!
//! Because task slots are recycled, lazy pruning keys on liveness, not on a
//! generation counter: a slot that is immediately reused by a new task may
//! briefly keep the old names visible until the new owner registers or the
//! lease expires. Slot generations are the documented follow-up.
//!
//! # Stats
//!
//! [`stats`] reports the live entry/lease counts plus cumulative
//! registrations, resolutions, unregistrations, lease expirations and
//! owner-death releases, so the test harness can assert on registry behaviour
//! without reading the table. Exposing the counters through a native op is a
//! follow-up; `messengerctl list` reads [`list`] for now.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::ipc::credentials;
use crate::ipc::handles::{self, HandleKind};
use crate::task::process::{self, ProcessInfo};
use crate::task::{self, TaskState};

/// Registry interface id: the first eight bytes of the spec name
/// `os.lazy.messenger.registry.v1` (`docs/messenger.md` section 13), read as a
/// little-endian word so the constant is self-documenting.
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

/// Registry methods, matching the native op codes' documentation.
pub mod method {
    /// Publish a name for an endpoint.
    pub const REGISTER: u32 = 1;
    /// Look a name up; the caller receives a handle to its endpoint.
    pub const RESOLVE: u32 = 2;
    /// Withdraw a name (owner, or `CAP_IPC_CONTROL`).
    pub const UNREGISTER: u32 = 3;
    /// Snapshot the table.
    pub const LIST: u32 = 4;
}

/// Field ids of the registry request/reply TLV bodies. The userspace mirror in
/// `user/src/messenger.rs` keeps the same numbers.
pub mod field {
    /// Request and list-record: the service name (string).
    pub const NAME: u16 = 1;
    /// Request and list-record: interfaces the service implements (an array
    /// whose nested items are `u64` fields carrying this same id).
    pub const INTERFACES: u16 = 2;
    /// Request: lease length in ticks; `0` means permanent.
    pub const LEASE_TICKS: u16 = 3;
    /// Request: the endpoint handle to publish (a number in the owner's table).
    pub const ENDPOINT: u16 = 4;
    /// List-record: the entry's kernel object id (diagnostic).
    pub const OBJECT: u16 = 5;
    /// List-record: the owner task slot.
    pub const OWNER: u16 = 6;
    /// List-record: remaining lease ticks (`0` when permanent).
    pub const LEASE_REMAINING: u16 = 7;
    /// List body: one record per entry.
    pub const ENTRY: u16 = 8;
    /// Register/resolve reply: the handle now open in the caller's table.
    pub const HANDLE: u16 = 9;
}

/// Largest name the registry accepts, in bytes.
pub const MAX_NAME_BYTES: usize = 128;
/// Largest number of interfaces one registration may declare.
pub const MAX_INTERFACES: usize = 16;
/// Largest number of live names.
pub const MAX_ENTRIES: usize = 64;

/// Why a registry operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The name is empty, too long, or contains a control character.
    BadName,
    /// The endpoint handle does not name a channel or endpoint.
    BadEndpoint,
    /// The name is already registered by someone else.
    NameTaken,
    /// No name by that spelling is registered.
    UnknownName,
    /// The actor may not unregister this name.
    NotOwner,
    /// More interfaces than [`MAX_INTERFACES`].
    TooManyInterfaces,
    /// The registry is full.
    RegistryFull,
    /// The target task slot holds no live task.
    BadTask,
    /// The target task is out of handles or the kernel is out of memory.
    NoResources,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::BadName => "that service name is empty, too long, or has control characters",
            Error::BadEndpoint => "a service name must refer to a channel or endpoint handle",
            Error::NameTaken => "that service name is already registered by another owner",
            Error::UnknownName => "no service is registered under that name",
            Error::NotOwner => "only the owner (or an administrator) may unregister that name",
            Error::TooManyInterfaces => "the registration declares too many interfaces",
            Error::RegistryFull => "the kernel name registry is full",
            Error::BadTask => "no task exists in that slot",
            Error::NoResources => "the target process is out of Messenger handles",
        }
    }
}

/// One registered name.
struct Entry {
    /// Service name (validated, at most [`MAX_NAME_BYTES`] bytes).
    name: String,
    /// Kind of the registered endpoint, recorded so `resolve` opens the same
    /// capability.
    kind: HandleKind,
    /// Rights the resolver receives; the owner chooses what a name grants.
    rights: u32,
    /// Kernel object the registered handle names (opaque to userspace).
    object_id: u64,
    /// Task slot that published the name.
    owner_slot: usize,
    /// Interface ids the service implements.
    interfaces: Vec<u64>,
    /// Absolute lease deadline in PIT ticks; `None` is permanent.
    lease_deadline: Option<u64>,
}

/// A table row copied out for `List`; the fabric never hands out references.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntryInfo {
    /// Service name.
    pub name: String,
    /// Kernel object the name refers to (diagnostic; not a usable handle).
    pub object_id: u64,
    /// Task slot that published the name.
    pub owner_slot: usize,
    /// Interface ids the service implements.
    pub interfaces: Vec<u64>,
    /// Remaining lease in ticks; `None` when permanent.
    pub lease_remaining: Option<u64>,
}

/// Live depths plus cumulative counters, for `messengerctl` and tests.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    /// Live registered names.
    pub entries: u64,
    /// Live names that carry a lease.
    pub leases: u64,
    /// Registrations accepted since boot (replacements included).
    pub registrations: u64,
    /// Resolutions that returned a handle since boot.
    pub resolves: u64,
    /// Explicit unregistrations since boot.
    pub unregistrations: u64,
    /// Entries dropped because their lease expired.
    pub expirations: u64,
    /// Entries dropped because their owner died (`release_owner`).
    pub releases: u64,
}

/// The name table and its counters, one lock so a mutation and its meter are
/// always consistent.
struct Registry {
    entries: Vec<Entry>,
    stats: Stats,
}

/// The single registry. A linear scan is right for [`MAX_ENTRIES`] names and
/// matches the channel registry's shape.
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    entries: Vec::new(),
    stats: Stats {
        entries: 0,
        leases: 0,
        registrations: 0,
        resolves: 0,
        unregistrations: 0,
        expirations: 0,
        releases: 0,
    },
});

/// Validate one name. Control characters would let a name forge log lines or
/// terminal output, so the accepted alphabet is printable ASCII.
fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(Error::BadName);
    }
    if !name.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(Error::BadName);
    }
    Ok(())
}

/// Snapshot the process table once and answer liveness for every slot, so a
/// prune pass takes the task lock once instead of once per entry. A live owner
/// is a task that still occupies its slot and has not exited; slots without a
/// task and `Done` tasks are dead, and the kernel task always counts as live.
fn live_slots() -> Vec<bool> {
    let mut live = alloc::vec![false; task::MAX_TASKS];
    if let Some(kernel) = live.get_mut(task::KERNEL_TASK) {
        *kernel = true;
    }
    for ProcessInfo { slot, state, .. } in process::process_list() {
        if let Some(entry) = live.get_mut(slot) {
            *entry = state != TaskState::Done;
        }
    }
    live
}

/// Whether `now` is past an entry's lease, and how many ticks remain.
fn lease_state(entry: &Entry, now: u64) -> (bool, Option<u64>) {
    match entry.lease_deadline {
        None => (false, None),
        Some(deadline) => (deadline <= now, Some(deadline.saturating_sub(now))),
    }
}

/// Drop dead-owner and expired entries, returning how many were removed.
///
/// `live` is the liveness snapshot the caller took before locking the registry,
/// so the task table is never locked while the registry lock is held.
fn prune_locked(registry: &mut Registry, live: &[bool], now: u64) -> usize {
    let mut expired = 0u64;
    let mut released = 0u64;
    let before = registry.entries.len();
    registry.entries.retain(|entry| {
        if !live.get(entry.owner_slot).copied().unwrap_or(false) {
            released += 1;
            return false;
        }
        if lease_state(entry, now).0 {
            expired += 1;
            return false;
        }
        true
    });
    registry.stats.expirations += expired;
    registry.stats.releases += released;
    before - registry.entries.len()
}

/// Prune at the current tick ([`task::ticks`]).
pub fn prune() -> usize {
    prune_at(task::ticks())
}

/// Prune as if the clock read `now`; deterministic tests use this directly.
pub fn prune_at(now: u64) -> usize {
    let live = live_slots();
    let mut registry = REGISTRY.lock();
    prune_locked(&mut registry, &live, now)
}

/// Publish `name` for the endpoint `object_id` (owned by `owner_slot`).
///
/// The owner is recorded from the caller's slot, not from a user field, so
/// ownership cannot be forged. Re-registering a name the same owner holds
/// replaces it (a service restart); another owner gets [`Error::NameTaken`].
/// `lease_ticks` of `0` registers a permanent name, anything else a lease that
/// expires that many ticks after this call.
pub fn register(
    owner_slot: usize,
    name: &str,
    kind: HandleKind,
    rights: u32,
    object_id: u64,
    interfaces: &[u64],
    lease_ticks: u64,
) -> Result<(), Error> {
    validate_name(name)?;
    if !matches!(kind, HandleKind::Channel | HandleKind::Endpoint) {
        return Err(Error::BadEndpoint);
    }
    if interfaces.len() > MAX_INTERFACES {
        return Err(Error::TooManyInterfaces);
    }
    let now = task::ticks();
    let live = live_slots();
    let mut registry = REGISTRY.lock();
    prune_locked(&mut registry, &live, now);
    if let Some(index) = registry.entries.iter().position(|entry| entry.name == name) {
        if registry.entries[index].owner_slot != owner_slot {
            return Err(Error::NameTaken);
        }
        // Same owner: replace, so a restarting service keeps its name.
        registry.entries.remove(index);
    } else if registry.entries.len() >= MAX_ENTRIES {
        return Err(Error::RegistryFull);
    }
    let lease_deadline = (lease_ticks != 0).then(|| now + lease_ticks);
    registry.entries.push(Entry {
        name: String::from(name),
        kind,
        rights,
        object_id,
        owner_slot,
        interfaces: interfaces.to_vec(),
        lease_deadline,
    });
    registry.stats.registrations += 1;
    Ok(())
}

/// Resolve `name` and open a handle to its endpoint in `target_slot`'s table.
///
/// This is the capability transfer at the heart of discovery: the caller's
/// table gains a fresh number for an object it never had; the returned handle
/// carries the rights the owner published. `messengerd` uses the same entry
/// point with the requesting task's slot as `target_slot` (its
/// `CAP_IPC_CONTROL` capability is checked by the syscall edge), so a client
/// receives the handle without ever naming another process's number.
///
/// Note the channel model: all resolvers alias the *same* endpoint, and
/// closing an endpoint closes that side for everyone (`channels::close_endpoint`
/// is channel-scoped). A resolved handle must therefore stay open for the life
/// of the task; closing one is peer death for the service. Per-connection
/// channels are the documented follow-up.
///
/// A name whose endpoint closed without being unregistered still resolves; the
/// handle is valid but names a dead channel, so the caller's first operation
/// reports the closed peer. Owner teardown and lease expiry are what remove
/// such a name automatically.
pub fn resolve(target_slot: usize, name: &str) -> Result<u64, Error> {
    validate_name(name)?;
    let now = task::ticks();
    let live = live_slots();
    let opened = {
        let mut registry = REGISTRY.lock();
        prune_locked(&mut registry, &live, now);
        let entry = registry
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or(Error::UnknownName)?;
        (entry.kind, entry.rights, entry.object_id)
    };
    let handle =
        handles::open_for_task(target_slot, opened.0, opened.1, opened.2).map_err(|error| {
            use handles::Error::*;
            match error {
                NoFreeHandle => Error::NoResources,
                BadTask => Error::BadTask,
                InvalidHandle | MissingRight => Error::NoResources,
            }
        })?;
    // Another pass may have unregistered or re-registered the name between the
    // two locks; the handle still names a valid object, so count the resolve.
    REGISTRY.lock().stats.resolves += 1;
    Ok(handle)
}

/// Unregister `name` on behalf of `owner_slot`.
///
/// `actor_slot` is the caller making the request; it may be `owner_slot`, or a
/// different slot only while holding `CAP_IPC_CONTROL` (the administrator
/// override). The owner recorded at registration is what is checked, so a
/// proxy such as `messengerd` unregisters names for their real owners.
pub fn unregister(actor_slot: usize, owner_slot: usize, name: &str) -> Result<(), Error> {
    validate_name(name)?;
    let authorised = actor_slot == owner_slot
        || credentials::of(actor_slot).has_cap(credentials::CAP_IPC_CONTROL);
    let now = task::ticks();
    let live = live_slots();
    let mut registry = REGISTRY.lock();
    prune_locked(&mut registry, &live, now);
    let index = registry
        .entries
        .iter()
        .position(|entry| entry.name == name)
        .ok_or(Error::UnknownName)?;
    if registry.entries[index].owner_slot != owner_slot || !authorised {
        return Err(Error::NotOwner);
    }
    registry.entries.remove(index);
    registry.stats.unregistrations += 1;
    Ok(())
}

/// Drop every name `slot` owns: the owner-death teardown hook.
///
/// Returns how many names were released. The registry is not wired into
/// `task/**` teardown while that module is frozen for this issue; the tests
/// call it, and the task teardown loop will call it when the freeze lifts.
/// Access-time pruning still catches an owner that died without the hook.
pub fn release_owner(slot: usize) -> usize {
    let mut registry = REGISTRY.lock();
    let before = registry.entries.len();
    registry.entries.retain(|entry| entry.owner_slot != slot);
    let removed = before - registry.entries.len();
    registry.stats.releases += removed as u64;
    removed
}

/// Snapshot the table (pruning first), oldest registration first.
pub fn list() -> Vec<EntryInfo> {
    let now = task::ticks();
    let live = live_slots();
    let mut registry = REGISTRY.lock();
    prune_locked(&mut registry, &live, now);
    registry
        .entries
        .iter()
        .map(|entry| EntryInfo {
            name: entry.name.clone(),
            object_id: entry.object_id,
            owner_slot: entry.owner_slot,
            interfaces: entry.interfaces.clone(),
            lease_remaining: lease_state(entry, now).1,
        })
        .collect()
}

/// Live depths plus cumulative counters. Prunes first so `entries`/`leases`
/// never report a name that a resolve would refuse.
pub fn stats() -> Stats {
    let now = task::ticks();
    let live = live_slots();
    let mut registry = REGISTRY.lock();
    prune_locked(&mut registry, &live, now);
    let mut stats = registry.stats;
    stats.entries = registry.entries.len() as u64;
    stats.leases = registry
        .entries
        .iter()
        .filter(|entry| entry.lease_deadline.is_some())
        .count() as u64;
    stats
}

/// Drop the whole table and its counters (test isolation, reboot). A running
/// system never clears its registry.
pub fn reset() {
    let mut registry = REGISTRY.lock();
    registry.entries.clear();
    registry.stats = Stats::default();
}

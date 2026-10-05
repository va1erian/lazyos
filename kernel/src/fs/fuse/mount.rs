//! Registering, unregistering and reaping providers: the mount edge of
//! [`super`]. A provider's mount lives in both mount tables (native and
//! Linux ABI) at `/mnt/<name>`, always `nosuid`.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use super::{FuseError, FuseFs, Phase, Slot, Stats, REAP, SLOTS};
use crate::fs::vfs::{FileKind, Filesystem, FsError, Id, MountFlags, Vfs};
use fused::wire::MAX_PAYLOAD;

/// The mount point of provider name `name`.
pub(super) fn point_of(name: &str) -> String {
    alloc::format!("{}/{name}", fhs::mount::MNT)
}

/// Register task `owner` as the provider of `/mnt/<name>` and mount it in
/// both tables. Returns the provider id.
pub fn register(owner: usize, name: &str, flags: MountFlags) -> Result<usize, FuseError> {
    if !fused::wire::valid_mount_name(name.as_bytes()) {
        return Err(FuseError::Invalid);
    }
    // A dead provider of the same name goes first; a live one refuses.
    for slot in SLOTS.iter() {
        let (dead, live) = {
            let state = slot.state.lock();
            let same = state.registered && state.name == name;
            (same && !state.alive, same && state.alive)
        };
        if live {
            return Err(FuseError::Busy);
        }
        if dead && !unmount_slot(slot, true) {
            // A requester is still unwinding from it: the name frees soon.
            return Err(FuseError::Busy);
        }
    }
    let slot = claim(owner, name)?;
    let epoch = slot.state.lock().epoch;
    let fs: Arc<dyn Filesystem> = Arc::new(FuseFs::new(slot.index, epoch));
    let flags = flags.union(MountFlags {
        nosuid: true,
        ..MountFlags::default()
    });
    let point = point_of(name);
    if let Err(error) = mount_both(&point, fs, flags) {
        let mut state = slot.state.lock();
        state.alive = false;
        state.mounted = false;
        state.registered = false;
        return Err(error);
    }
    slot.state.lock().mounted = true;
    serial_println!(
        "fuse: {point} mounted by task {owner}{}",
        if flags.ro { " (ro)" } else { "" }
    );
    Ok(slot.index)
}

/// Take a free slot for `owner`, initialized whole under the one lock
/// acquisition that marks it registered: a slot is never seen registered
/// but not yet alive (which [`reap`] would take for a dead provider and
/// free under the new owner).
fn claim(owner: usize, name: &str) -> Result<&'static Slot, FuseError> {
    SLOTS
        .iter()
        .find(|slot| {
            let mut state = slot.state.lock();
            if state.registered {
                return false;
            }
            if state.bounce.len() != MAX_PAYLOAD {
                state.bounce.resize(MAX_PAYLOAD, 0);
            }
            state.owner = owner;
            state.name = String::from(name);
            state.alive = true;
            state.mounted = false;
            state.busy = false;
            state.phase = Phase::Idle;
            state.timeouts = 0;
            state.stats = Stats::default();
            state.epoch += 1;
            // Tags carry the slot, so one provider's tag never matches another's.
            state.next_tag = (slot.index as u64) << 56;
            state.registered = true;
            true
        })
        .ok_or(FuseError::Full)
}

/// Mount `fs` at `point` in the native and the Linux ABI tables, or in
/// neither.
fn mount_both(point: &str, fs: Arc<dyn Filesystem>, flags: MountFlags) -> Result<(), FuseError> {
    let mount = |vfs: &mut Vfs, fs: Arc<dyn Filesystem>| -> Result<(), FuseError> {
        match vfs.stat(Id::ROOT, fhs::mount::MNT) {
            Ok(meta) if meta.kind == FileKind::Dir => {}
            _ => return Err(FuseError::NoMountRoot),
        }
        vfs.mount(point, fs, flags).map_err(|_| FuseError::Busy)
    };
    crate::fs::with(|vfs| mount(vfs, Arc::clone(&fs))).unwrap_or(Err(FuseError::NoMountRoot))?;
    let abi = crate::fs::abi_with(|vfs| mount(vfs, fs)).unwrap_or(Err(FuseError::NoMountRoot));
    if abi.is_err() {
        let _ = crate::fs::with(|vfs| vfs.unmount(point));
    }
    abi
}

/// Take `slot`'s mount out of both tables (waiting for them when `wait`,
/// trying once otherwise), then free the slot unless a requester still
/// holds its request slot. False when something is left for [`reap`].
fn unmount_slot(slot: &Slot, wait: bool) -> bool {
    let (name, mounted) = {
        let state = slot.state.lock();
        (state.name.clone(), state.mounted)
    };
    if mounted {
        let point = point_of(&name);
        let unmount = |vfs: &mut Vfs| match vfs.unmount(&point) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(error) => Err(error),
        };
        let (native, abi) = if wait {
            (crate::fs::with(unmount), crate::fs::abi_with(unmount))
        } else {
            (
                crate::fs::try_with(unmount),
                crate::fs::try_abi_with(unmount),
            )
        };
        // A busy table is retried; one that does not exist has nothing to
        // unmount only when waiting told us so (`with` answers `None`).
        if !wait && (native.is_none() || abi.is_none()) {
            return false;
        }
        slot.state.lock().mounted = false;
        serial_println!("fuse: {point} unmounted");
    }
    free_if_idle(slot)
}

/// Give a dead, unmounted slot back, unless a requester still unwinds from
/// it: the slot (and its epoch) must not change under that requester.
fn free_if_idle(slot: &Slot) -> bool {
    let mut state = slot.state.lock();
    if state.busy {
        REAP.store(true, Ordering::Release);
        return false;
    }
    state.registered = false;
    state.alive = false;
    true
}

/// The daemon unmounts: pending and future requests fail, and the mount
/// leaves both tables before this returns.
pub fn unregister(id: usize, owner: usize) -> Result<(), FuseError> {
    let slot = SLOTS.get(id).ok_or(FuseError::NotOwner)?;
    {
        let mut state = slot.state.lock();
        if !state.registered || state.owner != owner || !state.alive {
            return Err(FuseError::NotOwner);
        }
        state.alive = false;
    }
    // Wake first: a requester parked on this provider holds a mount table,
    // and must let go of it before the unmount can take it.
    slot.wake_all();
    unmount_slot(slot, true);
    Ok(())
}

/// Task `slot` is gone: every provider it served dies; [`reap`] unmounts.
pub fn teardown_task(task_slot: usize) {
    for slot in SLOTS.iter() {
        let died = {
            let mut state = slot.state.lock();
            let died = state.registered && state.alive && state.owner == task_slot;
            if died {
                state.alive = false;
            }
            died
        };
        if died {
            let name = slot.state.lock().name.clone();
            serial_println!("fuse: /mnt/{name}: provider task {task_slot} died");
            REAP.store(true, Ordering::Release);
            slot.wake_all();
        }
    }
}

/// Unmount and free every dead provider, without waiting for a busy mount
/// table (the periodic flusher calls this; whatever is left is retried).
pub fn reap() {
    if !REAP.swap(false, Ordering::AcqRel) {
        return;
    }
    let mut pending = false;
    for slot in SLOTS.iter() {
        let dead = {
            let state = slot.state.lock();
            state.registered && !state.alive
        };
        if dead && !unmount_slot(slot, false) {
            pending = true;
        }
    }
    if pending {
        REAP.store(true, Ordering::Release);
    }
}

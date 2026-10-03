//! Late mounts: the home volume on storage that appears after boot, a USB
//! stick served by `usbd` (docs/architecture/usb-storage.md).
//!
//! At boot [`super::mounts`] looks for the configured home volume
//! (`home=LABEL=...` or `home=UUID=...` in `lazyos.cfg`) on the disks the
//! kernel drives. When it is not there, the request is kept here, `/home`
//! starts as a plain directory on `/`, and [`settle`] (syscall 33 op 4,
//! called by `init`) finishes the job once a provider has registered its
//! disk: each new provider disk has its MBR scanned, and the first ext2
//! volume on a provider disk or partition that carries the configured label
//! or UUID is mounted at `/home` in both mount tables, with the flags the
//! boot mount would have had. Other sticks stay registered block devices,
//! never mounted.
//!
//! `init` waits (bounded) for this before it starts the services and
//! sessions that use `/home`, so in practice nothing has a file open there
//! yet. Whatever did touch `/home` on the root before is shadowed by the
//! mount, as on any Unix.
//!
//! Nothing is read before the provider reports its boot-time scan done
//! ([`provider_scanned`]): until then it is still bringing devices up (a
//! stick's TEST UNIT READY alone may take seconds) rather than serving
//! requests, and a read would only time out and count against the disk.
//!
//! Every disk read here happens with no filesystem lock held: the volume is
//! opened and its orphans reclaimed before it is mounted.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::bootcfg::VolumeId;
use super::vfs::{Filesystem, MountFlags};
use crate::block::{self, partition, provider};

/// What [`settle`] found (the syscall's return value).
pub mod state {
    /// Nothing to do: no home volume configured, or it mounted at boot.
    pub const NONE: u32 = 0;
    /// This call mounted the home volume.
    pub const MOUNTED: u32 = 1;
    /// The home volume is configured but not found yet.
    pub const WAITING: u32 = 2;
    /// An earlier call mounted it.
    pub const MOUNTED_EARLIER: u32 = 3;
    /// Not found, and every provider has finished its first scan of the
    /// devices present at boot: waiting longer would not help.
    pub const ABSENT: u32 = 4;
}

/// The home volume the boot could not find, and its configured flags.
static PENDING: Mutex<Option<(VolumeId, MountFlags)>> = Mutex::new(None);
/// Set once [`settle`] mounted it.
static MOUNTED: AtomicBool = AtomicBool::new(false);
/// Set once a provider reported its boot-time scan done ([`provider_scanned`]).
static SCANNED: AtomicBool = AtomicBool::new(false);

/// A provider has registered every disk present when it started (syscall
/// 33 op 5): from now on a home volume that is not found is [`state::ABSENT`].
pub fn provider_scanned() {
    SCANNED.store(true, Ordering::Release);
}

/// Remember a configured home volume that was not found at boot.
pub(super) fn set_pending(id: VolumeId, flags: MountFlags) {
    *PENDING.lock() = Some((id, flags));
}

/// Scan new provider disks and mount the pending home volume if one of them
/// carries it. Returns a [`state`] code.
pub fn settle() -> u32 {
    let Some((id, flags)) = *PENDING.lock() else {
        return if MOUNTED.load(Ordering::Acquire) {
            state::MOUNTED_EARLIER
        } else {
            state::NONE
        };
    };
    if !SCANNED.load(Ordering::Acquire) {
        return state::WAITING;
    }
    for disk in provider::take_unscanned() {
        partition::scan_disk(disk);
    }
    let candidates: Vec<_> = block::devices()
        .into_iter()
        .filter(|device| provider::is_provider_device(device.name()))
        .collect();
    let found = super::mounts::find_ext2(&candidates, None, |volume| match id {
        VolumeId::Uuid(uuid) => volume.uuid() == uuid,
        VolumeId::Label(label) => volume.label() == label,
    });
    let Some((volume, device)) = found else {
        return state::ABSENT;
    };
    // Another `settle` may have won while this one was reading the disk.
    if PENDING.lock().take().is_none() {
        return state::MOUNTED_EARLIER;
    }
    super::mounts::reclaim_orphans(&volume, fhs::mount::HOME);
    let flags = flags.union(MountFlags {
        nosuid: true,
        ..Default::default()
    });
    let volume: Arc<dyn Filesystem> = Arc::new(volume);
    let native = super::with(|vfs| vfs.mount(fhs::mount::HOME, Arc::clone(&volume), flags));
    let abi = super::abi_with(|vfs| vfs.mount(fhs::mount::HOME, volume, flags));
    if !matches!(native, Some(Ok(()))) || !matches!(abi, Some(Ok(()))) {
        serial_println!("fs: could not mount {} at /home", device.name());
        return state::WAITING;
    }
    MOUNTED.store(true, Ordering::Release);
    serial_println!(
        "fs: mounted {} at /home (late, home volume {}){}",
        device.name(),
        super::mounts::describe(&id),
        flags.proc_suffix()
    );
    state::MOUNTED
}

/// Forget the pending request and the mounted flag (test isolation).
#[cfg(lazyos_tests)]
pub fn reset_for_tests(pending: Option<(VolumeId, MountFlags)>) {
    *PENDING.lock() = pending;
    MOUNTED.store(false, Ordering::Release);
    SCANNED.store(false, Ordering::Release);
}

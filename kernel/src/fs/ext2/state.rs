//! The clean/dirty superblock state and the write ordering around it.
//!
//! ext2 has no journal, so an interrupted stop can leave the bitmaps, counters
//! and inodes disagreeing. `s_state` is how a later mount finds out: the
//! volume is marked *dirty* before its first change reaches the disk and
//! marked *clean* only after everything else is durable. The two rules:
//!
//! * dirty first: the marker is written and flushed before any other write, so
//!   there is never a moment where changed metadata sits behind a clean flag;
//! * clean last: the device is flushed *before* the clean marker is written,
//!   so a power cut can never persist "clean" ahead of the data it vouches for.
//!
//! The marker is per-mount, not per-write: after the first change the volume
//! stays dirty until [`Ext2::sync_volume`], so steady-state writes pay nothing.

use core::sync::atomic::Ordering;

use super::*;

impl Ext2 {
    /// Log what the previous stop left behind. A mount never repairs anything
    /// (no fsck here); it only makes the situation visible.
    pub(super) fn report_mount_state(device: &str, state: u16) {
        if state & STATE_VALID == 0 {
            crate::serial_println!("ext2: {device} was not cleanly unmounted (unclean stop)");
        }
        if state & STATE_ERROR != 0 {
            crate::serial_println!("ext2: {device} has recorded filesystem errors");
        }
    }

    /// Whether the volume was flagged clean when it was mounted.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // diagnostics/tests
    pub fn was_clean_at_mount(&self) -> bool {
        self.mount_state & STATE_VALID != 0
    }

    /// Persist `s_state = state` (and the write time) in the superblock.
    fn store_state(&self, state: u16) -> Result<(), FsError> {
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        put16(&mut raw, SB_STATE, state);
        put32(&mut raw, SB_WTIME, now());
        self.write_super_raw(&raw)
    }

    /// Flag the volume dirty, durably, before the first change of a session.
    ///
    /// Called from every block write (with `lock` held). The flag flips first
    /// so the superblock write below does not re-enter this path, and flips
    /// back if the marker cannot be made durable: then the mutation that
    /// asked for it fails instead of proceeding behind a clean flag.
    pub(super) fn mark_dirty(&self) -> Result<(), FsError> {
        if !self.clean.swap(false, Ordering::Relaxed) {
            return Ok(());
        }
        let marked = self
            .store_state(self.mount_state & !STATE_VALID)
            .and_then(|()| self.device.flush().map_err(io_error));
        if marked.is_err() {
            self.clean.store(true, Ordering::Relaxed);
        }
        marked
    }

    /// Make every earlier write durable, then mark the volume clean.
    ///
    /// The clean marker is written only when this mount had dirtied the volume,
    /// and it restores the state found at mount: a volume that arrived unclean
    /// (or with recorded errors) is not laundered by our own shutdown.
    pub(super) fn sync_volume(&self) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        self.device.flush().map_err(io_error)?; // 1. data and metadata durable
        if self.read_only || self.clean.load(Ordering::Relaxed) {
            return Ok(()); // nothing of ours to vouch for
        }
        self.store_state(self.mount_state)?; // 2. the clean marker...
        self.device.flush().map_err(io_error)?; // 3. ...made durable
        self.clean
            .store(self.mount_state & STATE_VALID != 0, Ordering::Relaxed);
        Ok(())
    }
}

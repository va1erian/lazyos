//! The raw-claim syscalls (ops 6-11 of syscall 33): who may call them (a
//! filesystem provider started from `fatfuse`'s program, and no other
//! program with the same capability and uid), and hostile arguments.

use super::lifecycle::{daemon_task, disk_id, failed, generation_of, owner};
use super::*;
use crate::block::provider::rawsys::op;
use crate::block::provider::sys as storage;
use crate::block::provider::MAX_REQUEST_BYTES;
use crate::ipc::credentials::CAP_FS_PROVIDER;
use crate::user_ptr;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EBUSY: i64 = 16;
const ENODEV: i64 = 19;
const EINVAL: i64 = 22;
const ESTALE: i64 = 116;

pub fn raw_syscalls_are_gated_and_checked() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let generation = generation_of(id).ok_or("not listed")?;
        let call = |operation, a1, a2, a3, a4| storage::dispatch(operation, a1, a2, a3, a4);
        let id64 = id as u64;
        // The provider (`_usb`, CAP_BLOCK_PROVIDER) is not a daemon, nor is
        // another uid with the capability, nor root without it.
        task::harness::switch_current(me);
        check!(
            call(op::CLAIM, id64, generation, 0, 0) == failed(EPERM),
            "the provider claimed"
        );
        let stranger = {
            task::harness::switch_current(task::KERNEL_TASK);
            let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
            credentials::set(slot, Cred::new(1000, 1000, CAP_FS_PROVIDER, 0, 0));
            task::harness::switch_current(slot);
            slot
        };
        check!(
            call(op::CLAIM, id64, generation, 0, 0) == failed(EPERM),
            "uid 1000 claimed"
        );
        let _ = stranger;
        // `ftpfuse` and `smbfuse` are `_mountd` with the capability too: the
        // program, not the identity, is what may claim a disk raw.
        let network = {
            task::harness::switch_current(task::KERNEL_TASK);
            let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
            credentials::set(
                slot,
                Cred::new(
                    mounttable::MOUNTD_UID,
                    mounttable::MOUNTD_UID,
                    CAP_FS_PROVIDER,
                    0,
                    0,
                ),
            );
            task::linuxstate::record_program(slot, fhs::bin::SMBFUSE);
            task::harness::switch_current(slot);
            slot
        };
        // Listing is the provider class's (`mountd` decides what to start
        // from it); the claim and everything after it is the program's.
        let mut seen = words_buf();
        check!(
            call(op::DISKS, seen.as_mut_ptr() as u64, 64, 0, 0) == 1,
            "a filesystem provider could not list the disks"
        );
        check!(
            call(op::READ, id64, 0, seen.as_mut_ptr() as u64, 512) == failed(EPERM),
            "smbfuse read a disk raw"
        );
        check!(
            call(op::CLAIM, id64, generation, 0, 0) == failed(EPERM),
            "smbfuse claimed a disk"
        );
        let _ = network;
        // Started by nothing a program was recorded for (a kernel-made task).
        task::harness::switch_current(task::KERNEL_TASK);
        let bare = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
        credentials::set(
            bare,
            Cred::new(
                mounttable::MOUNTD_UID,
                mounttable::MOUNTD_UID,
                CAP_FS_PROVIDER,
                0,
                0,
            ),
        );
        task::harness::switch_current(bare);
        check!(
            call(op::CLAIM, id64, generation, 0, 0) == failed(EPERM),
            "a task with no program claimed a disk"
        );
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            call(op::DISKS, 0, 0, 0, 0) == failed(EPERM),
            "the kernel task listed"
        );
        let _daemon = daemon_task()?;
        // The listing: one record of four words.
        let mut words = [0u64; 8];
        let listed = call(op::DISKS, words.as_mut_ptr() as u64, 64, 0, 0);
        check!(listed == 1, "listed {listed:#x}");
        check!(
            words[..4] == [id64, generation, SECTORS, 1],
            "the record {:x?}",
            &words[..4]
        );
        check!(
            call(op::DISKS, words.as_mut_ptr() as u64, 31, 0, 0) == 0,
            "a buffer too small for one record"
        );
        // Claim, then hostile reads and writes.
        check!(
            call(op::CLAIM, id64, generation + 1, 0, 0) == failed(ESTALE),
            "stale generation"
        );
        check!(
            call(op::CLAIM, 99, generation, 0, 0) == failed(ENOENT),
            "no such disk"
        );
        check!(call(op::CLAIM, id64, generation, 0, 0) == 0, "claim");
        check!(
            call(op::CLAIM, id64, generation, 0, 0) == failed(EBUSY),
            "claim twice"
        );
        let mut sector = vec![0u8; SECTOR_SIZE];
        let ptr = sector.as_mut_ptr() as u64;
        check!(
            call(op::READ, id64, 0, ptr, 100) == failed(EINVAL),
            "a partial sector"
        );
        check!(
            call(op::READ, id64, SECTORS, ptr, SECTOR_SIZE as u64) == failed(EINVAL),
            "past the end"
        );
        check!(
            call(op::READ, id64, 0, ptr, (MAX_REQUEST_BYTES + SECTOR_SIZE) as u64)
                == failed(EINVAL),
            "longer than a request"
        );
        let previous = user_ptr::set_trust_kernel_pointers(false);
        let bad = call(op::READ, id64, 0, 0x10, SECTOR_SIZE as u64);
        let bad_write = call(op::WRITE, id64, 0, 0x10, SECTOR_SIZE as u64);
        user_ptr::set_trust_kernel_pointers(previous);
        check!(bad == failed(14), "an unwritable buffer: {bad:#x}");
        check!(bad_write == failed(14), "an unreadable buffer: {bad_write:#x}");
        sector.fill(0xA7);
        check!(
            call(op::WRITE, id64, 5, ptr, SECTOR_SIZE as u64) == SECTOR_SIZE as u64,
            "write"
        );
        sector.fill(0);
        check!(
            call(op::READ, id64, 5, ptr, SECTOR_SIZE as u64) == SECTOR_SIZE as u64,
            "read"
        );
        check!(sector.iter().all(|&b| b == 0xA7), "read back other bytes");
        check!(call(op::FLUSH, id64, 0, 0, 0) == 0, "flush");
        check!(call(op::RELEASE, id64, 0, 0, 0) == 0, "release");
        check!(
            call(op::READ, id64, 5, ptr, SECTOR_SIZE as u64) == failed(EPERM),
            "read after release"
        );
        // A dead medium is ENODEV to a claimant, a read-only one EROFS.
        check!(call(op::CLAIM, id64, generation, 0, 0) == 0, "claim again");
        provider::remove(id, me).map_err(|e| format!("{e:?}"))?;
        check!(
            call(op::READ, id64, 5, ptr, SECTOR_SIZE as u64) == failed(ENODEV),
            "read of a dead medium"
        );
        Ok(())
    })();
    teardown();
    result
}

fn words_buf() -> alloc::boxed::Box<[u64; 8]> {
    alloc::boxed::Box::new([0u64; 8])
}

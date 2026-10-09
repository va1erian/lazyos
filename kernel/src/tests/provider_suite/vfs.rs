//! A provider never waits for the VFS (issue #704): a task waiting for its
//! provider holds a mount table (the native one, or the Linux ABI one for a
//! BusyBox shell), and a native mutation takes both, so a provider's native
//! fs call made meanwhile (`usbd` writing its `usb.dump`) would stall until
//! the request's deadline killed the disk. It gets `EAGAIN` instead, and
//! only while it serves a live disk and a table is held.

use super::*;
use crate::process;

const STAT: u64 = 15;
const EAGAIN: u64 = 11u64.wrapping_neg();

/// `stat("/")` as the current task.
fn stat_root() -> u64 {
    let path = b"/\0";
    let mut out = [0u64; 2];
    process::dispatch_for_test(STAT, path.as_ptr() as u64, out.as_mut_ptr() as u64, 0)
}

pub fn provider_never_waits_for_the_vfs() -> Result<(), String> {
    setup(Mode::Normal)?;
    let result = (|| {
        let owner = with_fake(|fake| fake.owner);
        check!(provider::serves_disk(owner), "the provider serves no disk");
        check!(stat_root() != EAGAIN, "a free VFS was refused");
        // Were this to wait, the test would hang here: the table is held
        // by this very task, as a requester holds it while it waits.
        for abi in [false, true] {
            let held = crate::fs::hold_vfs(abi, stat_root);
            check!(
                held == EAGAIN,
                "a provider's call with the table held (abi {abi}) gave {held:#x}"
            );
        }
        check!(stat_root() != EAGAIN, "the refusal outlived the hold");
        // A provider whose disk is gone is an ordinary task again.
        provider::remove(with_fake(|fake| fake.disk), owner)
            .map_err(|e| format!("remove: {e:?}"))?;
        check!(
            !provider::serves_disk(owner),
            "a removed disk is still served"
        );
        Ok(())
    })();
    teardown();
    result
}

/// Soak: thousands of calls alternating with and without the VFS held:
/// every held one is refused at once, every free one goes through, and no
/// frame leaks.
pub fn soak_provider_vfs_refusals() -> Result<(), String> {
    setup(Mode::Normal)?;
    let result = (|| {
        let before = crate::mem::frame_stats().live();
        for round in 0..4000u32 {
            if round % 2 == 0 {
                // Alternately the native and the Linux ABI table.
                let held = crate::fs::hold_vfs(round % 4 == 0, stat_root);
                check!(held == EAGAIN, "round {round}: held call gave {held:#x}");
            } else {
                let free = stat_root();
                check!(free != EAGAIN, "round {round}: free call refused");
            }
        }
        let after = crate::mem::frame_stats().live();
        check!(after <= before + 8, "frames leaked: {before} -> {after}");
        Ok(())
    })();
    teardown();
    result
}

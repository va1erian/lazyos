//! The fault-storm detector (issue #373): one report per run of identical
//! "handled" faults, reset by any different fault; plus the PTE walk it
//! reports.

use super::*;
use crate::arch::fault_storm::{self, Path};

/// A storm is reported exactly once, on the threshold fault; any other
/// fault in between restarts the count.
pub fn fault_storm_reports_once_per_storm() -> Result<(), String> {
    fault_storm::reset();
    let table = mem::kernel_table();
    let (rip, addr) = (0x40_1000u64, 0x4000_018cu64);
    // A different fault after 9999 repeats restarts the run.
    for _ in 0..9_999 {
        check!(
            !fault_storm::note(table, rip, addr, 0x6, Path::Demand),
            "reported before the threshold"
        );
    }
    check!(
        !fault_storm::note(table, rip + 1, addr, 0x6, Path::Demand),
        "a different rip counted as the same storm"
    );
    let mut reports = 0;
    for _ in 0..30_000 {
        if fault_storm::note(table, rip, addr, 0x7, Path::Cow) {
            reports += 1;
        }
    }
    check!(reports == 1, "{reports} reports for one storm");
    fault_storm::reset();
    Ok(())
}

/// The PTE chain of a mapped user page ends in a present leaf naming its
/// frame; an unmapped address stops at the first absent level.
pub fn pte_chain_walks_to_the_leaf() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let base = 0x0050_0000u64;
    process::map_range(table, base, base + 4096).map_err(|error| alloc::format!("{error:?}"))?;
    let chain = mem::pte_chain(table, base + 0x18c);
    let unmapped = mem::pte_chain(table, 0x7000_0000);
    mem::free_user_table(table);
    check!(
        chain.iter().all(|entry| entry & 1 != 0),
        "mapped page walk {chain:x?}"
    );
    check!(unmapped[3] == 0, "unmapped leaf {unmapped:x?}");
    Ok(())
}

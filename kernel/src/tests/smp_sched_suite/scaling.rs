//! The plan's scaling check (docs/smp-plan.md S8.3, acceptance): ring-3
//! tasks that only count scale with the CPUs that run them.

use alloc::vec::Vec;

use super::super::gs_suite::{spawn_user, Hostile};
use super::rig::*;
use super::*;
use crate::task::idle;

/// How long each phase counts.
const PHASE_NS: u64 = 400_000_000;

/// `inc qword ptr [rip + RESULT]`, then jump back: count forever.
#[rustfmt::skip]
const COUNTER: [u8; 9] = [
    0x48, 0xff, 0x05, 0xf9, 0x0e, 0x00, 0x00, // 0x00 inc qword [rip + 0xef9] (= 0xf00)
    0xeb, 0xf7, //                               0x07 jmp 0x00
];

/// The counters' sum over one phase of [`PHASE_NS`] while the suite sleeps.
fn phase(tasks: &[Hostile]) -> u64 {
    let start: u64 = tasks.iter().map(Hostile::count).sum();
    sleep_ns(PHASE_NS);
    let end: u64 = tasks.iter().map(Hostile::count).sum();
    end - start
}

/// One counting task per CPU. With the APs held back the boot CPU alone
/// runs them; with the APs let go every CPU does: the second phase counts
/// at least half as much again per extra CPU (linear would be N times).
pub fn parallel_counting_scales() -> Result<(), String> {
    with_aps(|aps| {
        let cpus = aps.len() + 1;
        let mut tasks = Vec::new();
        for _ in 0..cpus {
            tasks.push(spawn_user(&COUNTER)?);
        }
        idle::pause_aps(true);
        sleep_ns(20_000_000);
        let alone = phase(&tasks);
        idle::pause_aps(false);
        sleep_ns(20_000_000);
        let together = phase(&tasks);
        for hostile in &tasks {
            if let Some(pml4) = task::harness::pml4(hostile.slot) {
                crate::task::signal::terminate_process(pml4, 0);
            }
        }
        let reclaimed = nap_until(5_000_000_000, || {
            task::reclaim_pending();
            tasks
                .iter()
                .all(|hostile| task::harness::pml4(hostile.slot).is_none())
        });
        let ratio_x100 = together * 100 / alone.max(1);
        crate::trace::test!(
            "smp_sched_parallel_counting_scales:INFO:cpus={cpus} one_cpu={alone} all_cpus={together} ratio_x100={ratio_x100}"
        );
        check!(alone > 0, "nothing counted on the boot CPU alone");
        check!(
            ratio_x100 >= 100 + 50 * (cpus as u64 - 1),
            "{cpus} CPUs counted {ratio_x100}/100 times what one did"
        );
        check!(reclaimed, "the counting tasks were never reclaimed");
        Ok(())
    })
}

//! Soak for `execve`d native programs (see the parent module).

use super::*;

const SYS_WAIT4: u64 = 61;

/// Soak: hundreds of `sh -> fork -> native program -> wait4` cycles, half in
/// the foreground pattern (child reaps, shell reaps) and half with the job
/// killed by a signal-style status. Slots and frames must return to the
/// baseline (a leak would exhaust the 64-slot table or the frame pool), and
/// each generation's status must arrive intact.
pub fn soak_spawn_exit_cycles() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    let one_cycle = |generation: u64| -> Result<(), String> {
        let forked = task::spawn_child("fork", &service_suite::minimal_elf())
            .map_err(|e| format!("generation {generation}: fork: {e}"))?;
        task::harness::switch_current(forked);
        let args = format!("--gen {generation}");
        let program = native::spawn("TOP.ELF", &service_suite::minimal_elf(), &args)
            .map_err(|e| format!("generation {generation}: spawn errno {e}"))?;
        let want = if generation.is_multiple_of(2) {
            generation & 0x7f
        } else {
            128 + (generation & 0x1f)
        };
        task::harness::finish(program, want);
        let got = task::reap_child_slot(program)
            .ok_or_else(|| format!("generation {generation}: program not reapable"))?;
        if got != want {
            return Err(format!("generation {generation}: status {got} != {want}"));
        }
        task::harness::finish(forked, got & 0xff);
        task::harness::switch_current(sh);
        let mut status = 0u32;
        let ret = process::linux::dispatch_args_for_test(
            SYS_WAIT4,
            u64::MAX,
            &mut status as *mut u32 as u64,
            0,
            0,
        );
        if ret != forked as u64 || (status >> 8) as u64 != (want & 0xff) {
            return Err(format!(
                "generation {generation}: wait4 {ret:#x} status {status:#x}, want {forked}/{want}"
            ));
        }
        Ok(())
    };
    // Warm-up so interned names and lazily created tables do not count.
    one_cycle(0)?;
    let slots_before = task::free_slots();
    let frames_before = mem::frame_stats().live();
    for generation in 1..=384u64 {
        one_cycle(generation)?;
    }
    check!(
        task::free_slots() == slots_before,
        "task slots leaked: {} free, {} before",
        task::free_slots(),
        slots_before
    );
    check!(
        mem::frame_stats().live() <= frames_before,
        "live frames grew from {frames_before} to {}",
        mem::frame_stats().live()
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    Ok(())
}

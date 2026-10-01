//! Kernel entry with the direction flag set (issue #405).
//!
//! User code legitimately runs with `RFLAGS.DF` set (musl's `memmove` copies
//! backwards with `std; rep movsb; cld`), and a hardware gate does not clear
//! it. The kernel's `memcpy`/`memset` are `rep movs`/`rep stos` that assume
//! it clear, so every naked entry stub must `cld` before its Rust body runs.
//! `task::harness::note_entry_flags` (test builds) counts Rust-side entries
//! that still see DF set; these tests drive each gate with DF set and expect
//! that count to stay 0, while the caller's own DF survives the round trip
//! (`iretq` restores it).

use super::*;
use core::arch::asm;

/// `RFLAGS.DF`.
const DF: u64 = 1 << 10;

/// Fresh table with the kernel task current and runnable, so a real yield
/// from it resumes it (see `yield_clock`).
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    task::harness::take_entry_flag_stats();
}

/// Enter the voluntary-reschedule gate with DF set; returns RFLAGS after it.
///
/// Everything between `std` and `cld` is assembly: no Rust (and no compiler
/// generated `memcpy`) may run while DF is set.
fn yield_with_df() -> u64 {
    let rflags: u64;
    // SAFETY: `std`/`cld` bracket a single software interrupt through the
    // gate `arch::idt::init` installs at 0x81, which saves and restores every
    // register; `pushfq`/`pop` only read the flags. Interrupts are off, as the
    // real gate's callers have them.
    unsafe {
        asm!(
            "std",
            "int 0x81",
            "pushfq",
            "pop {flags}",
            "cld",
            flags = out(reg) rflags,
            options(nostack)
        );
    }
    rflags
}

/// Enter the native `int 0x80` gate (syscall 8, `clock`) with DF set;
/// returns RFLAGS after it.
fn syscall_with_df() -> u64 {
    let rflags: u64;
    // SAFETY: as `yield_with_df`; syscall 8 reads the tick counter and
    // touches nothing else. `rax` receives the result, the other argument
    // registers are saved and restored by the stub.
    unsafe {
        asm!(
            "std",
            "int 0x80",
            "pushfq",
            "pop {flags}",
            "cld",
            inout("rax") 8u64 => _,
            flags = out(reg) rflags,
            options(nostack)
        );
    }
    rflags
}

/// Take one real PIT tick with DF set: enable interrupts and halt until the
/// timer gate runs, then mask them again. Returns RFLAGS after it.
fn tick_with_df() -> u64 {
    let rflags: u64;
    // SAFETY: as `yield_with_df`, with the interrupt coming from the PIT
    // (`sti; hlt` is the same sleep `task::nap` uses); `cli` restores the
    // harness's interrupts-off state before any Rust runs again.
    unsafe {
        asm!(
            "std",
            "sti",
            "hlt",
            "cli",
            "pushfq",
            "pop {flags}",
            "cld",
            flags = out(reg) rflags,
            options(nostack)
        );
    }
    rflags
}

/// Each naked gate clears DF before its Rust body and restores the caller's
/// DF on the way out.
pub fn entry_clears_direction_flag() -> Result<(), String> {
    fresh();
    let flags = yield_with_df();
    check!(
        flags & DF != 0,
        "the yield gate did not restore the caller's DF"
    );
    let (checked, seen) = task::harness::take_entry_flag_stats();
    check!(checked >= 1, "the yield gate did not reach `schedule`");
    check!(
        seen == 0,
        "`schedule` ran with DF set after a yield ({seen}/{checked})"
    );

    let flags = syscall_with_df();
    check!(
        flags & DF != 0,
        "the syscall gate did not restore the caller's DF"
    );
    let (checked, seen) = task::harness::take_entry_flag_stats();
    check!(checked >= 1, "int 0x80 did not reach `syscall_dispatch`");
    check!(
        seen == 0,
        "`syscall_dispatch` ran with DF set ({seen}/{checked})"
    );

    let before = task::ticks();
    let flags = tick_with_df();
    check!(
        flags & DF != 0,
        "the timer gate did not restore the caller's DF"
    );
    check!(
        task::ticks() > before,
        "no PIT tick arrived during the halt"
    );
    let (checked, seen) = task::harness::take_entry_flag_stats();
    check!(checked >= 1, "the timer tick did not reach `schedule`");
    check!(
        seen == 0,
        "`schedule` ran with DF set after a tick ({seen}/{checked})"
    );
    check!(
        task::current() == task::KERNEL_TASK,
        "the only runnable task was not resumed: current is {}",
        task::current()
    );
    Ok(())
}

/// Soak: thousands of gate entries with DF set, plus a run of real ticks. A
/// single entry that reaches Rust with DF set fails the test.
pub fn entry_direction_flag_soak() -> Result<(), String> {
    const YIELDS: usize = 10_000;
    const SYSCALLS: usize = 2_000;
    const TICKS: usize = 20;
    fresh();
    for round in 0..YIELDS {
        check!(
            yield_with_df() & DF != 0,
            "round {round}: the yield gate lost the caller's DF"
        );
    }
    for round in 0..SYSCALLS {
        check!(
            syscall_with_df() & DF != 0,
            "round {round}: the syscall gate lost the caller's DF"
        );
    }
    for round in 0..TICKS {
        check!(
            tick_with_df() & DF != 0,
            "round {round}: the timer gate lost the caller's DF"
        );
    }
    let (checked, seen) = task::harness::take_entry_flag_stats();
    check!(
        checked >= (YIELDS + SYSCALLS + TICKS) as u64,
        "only {checked} gate entries reached Rust"
    );
    check!(
        seen == 0,
        "{seen} of {checked} kernel entries ran with DF set"
    );
    serial_println!("TEST:task_entry_direction_flag_soak:INFO:entries={checked}");
    Ok(())
}

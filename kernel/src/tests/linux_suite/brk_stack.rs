//! `brk` must never grow over another mapping. The Linux user stack
//! (`STACK_TOP - STACK_SIZE .. STACK_TOP`) sits inside the brk range, and
//! `vma::insert` overwrites what it covers, so an unchecked break turned the
//! live stack into heap: malloc handed out stack memory and a later shrink
//! unmapped it (issue #373).

use super::*;
use crate::mem::vma::{self, Kind, Prot};

const STACK_BOTTOM: u64 = process::linux::STACK_TOP - process::linux::STACK_SIZE;

fn brk(addr: u64) -> u64 {
    process::linux::dispatch_for_test(12, addr, 0, 0)
}

/// Record the stack VMA exactly as `elf::load` does (bookkeeping only; no
/// frames), run `body`, then drop the break and the stack again.
fn with_stack_vma(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    vma::insert(
        table,
        STACK_BOTTOM,
        process::linux::STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    );
    let outcome = body();
    brk(process::linux::BRK_BASE);
    vma::remove(table, STACK_BOTTOM, process::linux::STACK_TOP);
    outcome
}

/// The whole stack is still one `Stack` VMA.
fn stack_intact() -> Result<(), String> {
    let table = crate::mem::kernel_table();
    let found = vma::find_range(table, STACK_BOTTOM, process::linux::STACK_TOP);
    check!(
        found.len() == 1
            && found[0].kind == Kind::Stack
            && found[0].start == STACK_BOTTOM
            && found[0].end == process::linux::STACK_TOP,
        "stack VMA changed: {found:?}"
    );
    Ok(())
}

/// Growing up to the stack works; one page further leaves the break alone.
pub fn brk_stops_below_the_stack() -> Result<(), String> {
    with_stack_vma(|| {
        let base = process::linux::BRK_BASE;
        check!(brk(0) == base, "initial break {:#x}", brk(0));
        // Straight over the stack: refused, the break is unchanged.
        let code = brk(process::linux::STACK_TOP);
        check!(
            code == base,
            "brk(STACK_TOP) -> {code:#x}, expected {base:#x}"
        );
        stack_intact()?;
        // Right up to the stack: allowed.
        let code = brk(STACK_BOTTOM);
        check!(code == STACK_BOTTOM, "brk(stack bottom) -> {code:#x}");
        // One page into it: refused.
        let code = brk(STACK_BOTTOM + PAGE);
        check!(
            code == STACK_BOTTOM,
            "brk(stack bottom + page) -> {code:#x}, expected {STACK_BOTTOM:#x}"
        );
        stack_intact()?;
        // Shrinking never touches the stack either.
        check!(brk(base) == base, "shrink to base failed");
        stack_intact()
    })
}

/// Soak: many grow/shrink cycles across the stack boundary keep the stack
/// VMA whole and the break below it. The targets stay within a window around
/// the boundary (where the bug lives), so each shrink unmaps little.
pub fn brk_stack_boundary_soak() -> Result<(), String> {
    const WINDOW: u64 = 256;
    with_stack_vma(|| {
        let base = process::linux::BRK_BASE;
        let low = STACK_BOTTOM - WINDOW * PAGE;
        check!(brk(low) == low, "could not grow to the window");
        for round in 0..2000u64 {
            // Targets below, at, into and past the stack bottom.
            let target = low + ((round * 7919) % (2 * WINDOW)) * PAGE;
            let code = brk(target);
            if target <= STACK_BOTTOM {
                check!(
                    code == target,
                    "round {round}: brk({target:#x}) -> {code:#x}"
                );
            } else {
                check!(
                    code <= STACK_BOTTOM,
                    "round {round}: brk({target:#x}) -> {code:#x} crossed the stack"
                );
            }
            if round % 64 == 0 {
                stack_intact()?;
            }
        }
        check!(brk(base) == base, "final shrink failed");
        stack_intact()
    })
}

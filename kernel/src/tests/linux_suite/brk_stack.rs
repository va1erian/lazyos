//! `brk` must never grow over another mapping, and never past the layout's
//! ceiling. The stack used to sit inside the brk range (issue #373), and
//! `vma::insert` overwrites what it covers, so an unchecked break turned the
//! live stack into heap: malloc handed out stack memory and a later shrink
//! unmapped it. The stack now lives far above the break, but a `MAP_FIXED`
//! mapping can still land in the brk range, so the collision check stays: an
//! obstacle VMA stands in for it here.

use super::*;
use crate::mem::vma::{self, Kind, Prot};

/// Where the obstacle starts: a little above the scratch break.
const OBSTACLE: u64 = process::linux::BRK_BASE + 0x40_0000;
/// The obstacle's end.
const OBSTACLE_END: u64 = OBSTACLE + 0x10_0000;

fn brk(addr: u64) -> u64 {
    process::linux::dispatch_for_test(12, addr, 0, 0)
}

/// Record an obstacle VMA inside the brk range (bookkeeping only; no
/// frames), run `body`, then drop the break and the obstacle again.
fn with_obstacle(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    vma::insert(
        table,
        OBSTACLE,
        OBSTACLE_END,
        Prot::READ | Prot::WRITE,
        Kind::Anon,
    );
    let outcome = body();
    brk(process::linux::BRK_BASE);
    vma::remove(table, OBSTACLE, OBSTACLE_END);
    outcome
}

/// The whole obstacle is still one `Anon` VMA.
fn obstacle_intact() -> Result<(), String> {
    let table = crate::mem::kernel_table();
    let found = vma::find_range(table, OBSTACLE, OBSTACLE_END);
    check!(
        found.len() == 1
            && found[0].kind == Kind::Anon
            && found[0].start == OBSTACLE
            && found[0].end == OBSTACLE_END,
        "obstacle VMA changed: {found:?}"
    );
    Ok(())
}

/// Growing up to the obstacle works; one page further leaves the break alone.
pub fn brk_stops_below_the_stack() -> Result<(), String> {
    with_obstacle(|| {
        let base = process::linux::BRK_BASE;
        check!(brk(0) == base, "initial break {:#x}", brk(0));
        // Straight over the obstacle: refused, the break is unchanged.
        let code = brk(OBSTACLE_END + PAGE);
        check!(
            code == base,
            "brk(past obstacle) -> {code:#x}, expected {base:#x}"
        );
        obstacle_intact()?;
        // Right up to the obstacle: allowed.
        let code = brk(OBSTACLE);
        check!(code == OBSTACLE, "brk(obstacle) -> {code:#x}");
        // One page into it: refused.
        let code = brk(OBSTACLE + PAGE);
        check!(
            code == OBSTACLE,
            "brk(obstacle + page) -> {code:#x}, expected {OBSTACLE:#x}"
        );
        obstacle_intact()?;
        // Shrinking never touches the obstacle either.
        check!(brk(base) == base, "shrink to base failed");
        obstacle_intact()
    })
}

/// The break never reaches the mmap area or the stack: the layout's ceiling.
pub fn brk_stops_at_the_layout_ceiling() -> Result<(), String> {
    fresh()?;
    let base = process::linux::BRK_BASE;
    let ceiling = process::linux::BRK_LIMIT;
    let code = brk(ceiling + PAGE);
    check!(code == base, "brk(past the ceiling) -> {code:#x}");
    let code = brk(process::linux::STACK_TOP);
    check!(code == base, "brk(stack top) -> {code:#x}");
    // A break below the start is ignored, like Linux.
    let code = brk(base - PAGE);
    check!(code == base, "brk(below the start) -> {code:#x}");
    Ok(())
}

/// Soak: many grow/shrink cycles across the obstacle boundary keep the
/// obstacle VMA whole and the break below it. The targets stay within a
/// window around the boundary (where the bug lives), so each shrink unmaps
/// little.
pub fn brk_stack_boundary_soak() -> Result<(), String> {
    const WINDOW: u64 = 256;
    with_obstacle(|| {
        let base = process::linux::BRK_BASE;
        let low = OBSTACLE - WINDOW * PAGE;
        check!(brk(low) == low, "could not grow to the window");
        for round in 0..2000u64 {
            // Targets below, at, into and past the obstacle.
            let target = low + ((round * 7919) % (2 * WINDOW)) * PAGE;
            let code = brk(target);
            if target <= OBSTACLE {
                check!(
                    code == target,
                    "round {round}: brk({target:#x}) -> {code:#x}"
                );
            } else {
                check!(
                    code <= OBSTACLE,
                    "round {round}: brk({target:#x}) -> {code:#x} crossed the obstacle"
                );
            }
            if round % 64 == 0 {
                obstacle_intact()?;
            }
        }
        check!(brk(base) == base, "final shrink failed");
        obstacle_intact()
    })
}

//! Syscall 25 ops 7-9: the login console's keyboard claim (issue #396).

use super::*;
use crate::input::console;
use crate::input::keyboard;
use crate::input::rawsys::op;
use crate::ipc::credentials::{self, Cred, CAP_INPUT_CONSOLE, CAP_INPUT_RAW, CAP_SYS_ADMIN};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EBUSY: i64 = 16;

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn call(operation: u64) -> u64 {
    process::dispatch_for_test(25, operation, 0, 0)
}

/// A live scratch task with `caps`, made current.
fn task_with(caps: u32) -> Result<usize, String> {
    let slot = scratch()?;
    credentials::set(slot, Cred::new(0, 0, caps, 0, 0));
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Ask, as an `input.raw` holder (`inputd`), who holds the console.
fn owner_seen_by_inputd() -> Result<u64, String> {
    let current = task::current();
    let inputd = task_with(CAP_INPUT_RAW)?;
    let owner = call(op::CONSOLE_OWNER);
    task::harness::switch_current(current);
    let _ = inputd;
    Ok(owner)
}

fn reset() {
    console::reset();
    crate::display::reset();
    fresh();
}

/// Each op is refused without its capability: claiming and releasing need
/// `CAP_INPUT_CONSOLE` (root with every other bit is not enough), asking
/// who holds it needs `CAP_INPUT_RAW`.
pub fn capability_gate() -> Result<(), String> {
    reset();
    task_with(CAP_SYS_ADMIN | CAP_INPUT_RAW)?;
    check!(
        call(op::CONSOLE_CLAIM) == failed(EPERM),
        "claim without the bit"
    );
    check!(
        call(op::CONSOLE_RELEASE) == failed(EPERM),
        "release without the bit"
    );
    check!(console::holder().is_none(), "a refused claim took hold");
    task_with(CAP_INPUT_CONSOLE)?;
    check!(
        call(op::CONSOLE_OWNER) == failed(EPERM),
        "owner query without input.raw"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        call(op::CONSOLE_CLAIM) == failed(EPERM),
        "the kernel task claimed"
    );
    reset();
    Ok(())
}

/// Claim, a refused second claimant, reclaiming, a refused stranger's
/// release, release, and the next claimant.
pub fn lifecycle() -> Result<(), String> {
    reset();
    check!(
        owner_seen_by_inputd()? == failed(ENOENT),
        "a holder before any claim"
    );
    let first = task_with(CAP_INPUT_CONSOLE)?;
    check!(call(op::CONSOLE_CLAIM) == 0, "first claim refused");
    check!(
        owner_seen_by_inputd()? == first as u64,
        "owner is not the claimant"
    );
    check!(
        call(op::CONSOLE_CLAIM) == 0,
        "reclaim by the holder refused"
    );
    let second = task_with(CAP_INPUT_CONSOLE)?;
    check!(
        call(op::CONSOLE_CLAIM) == failed(EBUSY),
        "second claimant not refused"
    );
    check!(
        call(op::CONSOLE_RELEASE) == failed(EPERM),
        "a stranger released the claim"
    );
    task::harness::switch_current(first);
    check!(call(op::CONSOLE_RELEASE) == 0, "release refused");
    check!(
        owner_seen_by_inputd()? == failed(ENOENT),
        "a holder after release"
    );
    check!(
        call(op::CONSOLE_RELEASE) == failed(EPERM),
        "double release accepted"
    );
    task::harness::switch_current(second);
    check!(
        call(op::CONSOLE_CLAIM) == 0,
        "the freed console was not claimable"
    );
    check!(
        owner_seen_by_inputd()? == second as u64,
        "owner is not the new claimant"
    );
    reset();
    Ok(())
}

/// A holder that died, or lost the capability, holds nothing: its slot never
/// passes the claim on, and the next entitled task takes it over.
pub fn stale_holders_hold_nothing() -> Result<(), String> {
    reset();
    let dead = crate::task::MAX_TASKS - 1;
    check!(!task::live(dead), "slot {dead} unexpectedly live");
    check!(console::claim(dead).is_ok(), "seeding a dead holder");
    check!(console::holder().is_none(), "a dead slot holds the console");
    check!(!console::claimed(), "a dead holder still diverts keys");
    let taker = task_with(CAP_INPUT_CONSOLE)?;
    check!(
        call(op::CONSOLE_CLAIM) == 0,
        "a dead holder blocked the claim"
    );
    // Demoted: the same live slot without the bit no longer counts.
    credentials::set(taker, Cred::new(0, 0, 0, 0, 0));
    check!(
        console::holder().is_none(),
        "a demoted task holds the console"
    );
    let next = task_with(CAP_INPUT_CONSOLE)?;
    check!(
        call(op::CONSOLE_CLAIM) == 0,
        "a demoted holder blocked the claim"
    );
    check!(
        console::holder() == Some(next),
        "the takeover did not stick"
    );
    reset();
    Ok(())
}

/// While the console is claimed a typed character reaches the raw bus (for
/// `inputd`) and never the kernel terminal queue (where the console shell
/// would replay a password).
pub fn claimed_keys_stay_off_the_terminal() -> Result<(), String> {
    reset();
    let slot = task_with(CAP_INPUT_CONSOLE | CAP_INPUT_RAW)?;
    check!(call(op::OPEN) == 0, "raw open failed");
    check!(call(op::CONSOLE_CLAIM) == 0, "claim refused");
    let before = task::input_gen();
    for byte in [0x1E, 0x9E, 0x1C, 0x9C] {
        // 'a' down/up, Enter down/up.
        keyboard::push_scancode(byte);
    }
    check!(
        task::input_gen() == before,
        "a claimed key reached the terminal queue"
    );
    let records = drain_all(slot, 16)?;
    let keys = records.iter().filter(|r| r.kind == kind::KEY).count();
    check!(keys == 4, "the raw bus saw {keys} key edges, expected 4");
    check!(call(op::CONSOLE_RELEASE) == 0, "release refused");
    check!(!console::claimed(), "still claimed after release");
    bus::reset();
    reset();
    Ok(())
}

/// Many tasks claiming and releasing at random: the kernel's holder is always
/// exactly the model's (the last successful claimant, until it releases).
pub fn soak_claims() -> Result<(), String> {
    reset();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        tasks.push(task_with(CAP_INPUT_CONSOLE)?);
    }
    let mut model: Option<usize> = None;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for round in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let who = tasks[(state % 4) as usize];
        task::harness::switch_current(who);
        if state & 0x10 == 0 {
            let code = call(op::CONSOLE_CLAIM);
            let free = model.is_none_or(|holder| holder == who);
            check!(
                (code == 0) == free,
                "round {round}: claim by {who} -> {code:#x} with holder {model:?}"
            );
            if free {
                model = Some(who);
            }
        } else {
            let code = call(op::CONSOLE_RELEASE);
            let mine = model == Some(who);
            check!(
                (code == 0) == mine,
                "round {round}: release by {who} -> {code:#x} with holder {model:?}"
            );
            if mine {
                model = None;
            }
        }
        check!(console::holder() == model, "round {round}: holder drifted");
    }
    reset();
    Ok(())
}

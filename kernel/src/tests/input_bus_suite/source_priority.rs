//! An input driver's scheduling class: registering a source raises the
//! caller to `Interactive` (a polled USB device holds only a few events, and
//! a driver left behind a console repaint loses keys), never lowers a task
//! already above it, and a refused registration changes nothing. The stress
//! case repeats it over many generations, interleaved with refusals.

use super::source::{call, driver_task, failed, register};
use super::*;
use crate::input::rawsys::op;
use crate::input::sources::{self, class};
use crate::ipc::credentials::{CAP_INPUT_RAW, CAP_INPUT_SOURCE};
use crate::task::PriorityClass;

const EPERM: i64 = 1;
const EINVAL: i64 = 22;

fn clean() {
    sources::reset();
    fresh();
}

/// A current driver task with `caps`, demoted to `start`.
fn driver_in(caps: u32, start: PriorityClass) -> Result<usize, String> {
    let slot = driver_task(caps)?;
    check!(task::set_priority(slot, start), "slot {slot} has no task");
    Ok(slot)
}

fn class_of(slot: usize) -> Result<PriorityClass, String> {
    task::priority(slot).ok_or_else(|| format!("slot {slot} has no task"))
}

/// Normal and Background drivers become Interactive on register; a Realtime
/// one stays Realtime; refusals (no capability, bad class) raise nothing.
pub fn register_raises_driver() -> Result<(), String> {
    clean();
    for start in [PriorityClass::Background, PriorityClass::Normal] {
        let driver = driver_in(CAP_INPUT_SOURCE, start)?;
        register(class::KEYBOARD)?;
        let now = class_of(driver)?;
        check!(
            now == PriorityClass::Interactive,
            "a {} driver became {}",
            start.label(),
            now.label()
        );
        check!(
            task::weight(driver) == Some(PriorityClass::Interactive.default_weight()),
            "the raised driver kept its old weight"
        );
    }
    let realtime = driver_in(CAP_INPUT_SOURCE, PriorityClass::Realtime)?;
    register(class::POINTER)?;
    check!(
        class_of(realtime)? == PriorityClass::Realtime,
        "register lowered a realtime driver"
    );

    let refused = driver_in(CAP_INPUT_RAW, PriorityClass::Normal)?;
    let code = call(op::REGISTER_SOURCE, u64::from(class::KEYBOARD), 0);
    check!(
        code == failed(EPERM),
        "register without input.source -> {code:#x}"
    );
    check!(
        class_of(refused)? == PriorityClass::Normal,
        "a refused register raised the caller"
    );
    let bad = driver_in(CAP_INPUT_SOURCE, PriorityClass::Normal)?;
    let code = call(op::REGISTER_SOURCE, 0xFF, 0);
    check!(code == failed(EINVAL), "register(0xff) -> {code:#x}");
    check!(
        class_of(bad)? == PriorityClass::Normal,
        "a bad-class register raised the caller"
    );
    clean();
    Ok(())
}

/// Many generations: a Normal driver registers, is raised, closes and is
/// demoted again (a restarted driver), while a capability-less task and a
/// bad-class call are refused every time and stay Normal. The table never
/// fills, and the bystander's class never moves.
pub fn priority_stress_generations() -> Result<(), String> {
    const GENERATIONS: usize = 5_000;
    clean();
    let bystander = driver_in(0, PriorityClass::Normal)?;
    let intruder = driver_in(CAP_INPUT_RAW, PriorityClass::Normal)?;
    let driver = driver_in(CAP_INPUT_SOURCE, PriorityClass::Normal)?;
    for generation in 0..GENERATIONS {
        task::set_priority(driver, PriorityClass::Normal);
        task::harness::switch_current(driver);
        if generation % 2 == 1 {
            check!(
                call(op::REGISTER_SOURCE, 0xFF, 0) == failed(EINVAL),
                "generation {generation}: a bad class was accepted"
            );
            check!(
                class_of(driver)? == PriorityClass::Normal,
                "generation {generation}: a bad-class register raised the driver"
            );
        }
        let source_class = [class::KEYBOARD, class::POINTER, class::TABLET][generation % 3];
        let id = register(source_class)?;
        check!(
            class_of(driver)? == PriorityClass::Interactive,
            "generation {generation}: the driver was not raised"
        );
        check!(
            call(op::CLOSE_SOURCE, id, 0) == 0,
            "generation {generation}: close"
        );
        task::harness::switch_current(intruder);
        check!(
            call(op::REGISTER_SOURCE, u64::from(class::KEYBOARD), 0) == failed(EPERM),
            "generation {generation}: the intruder registered"
        );
        check!(
            class_of(intruder)? == PriorityClass::Normal,
            "generation {generation}: the intruder was raised"
        );
        check!(
            class_of(bystander)? == PriorityClass::Normal,
            "generation {generation}: a bystander's class moved"
        );
    }
    clean();
    Ok(())
}

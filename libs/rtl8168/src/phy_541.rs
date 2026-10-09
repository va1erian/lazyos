//! PHY steps specific to XID 541, and nothing else.
//!
//! The plan (docs/rtl8168-driver-plan.md section 3.2 step 6) starts from the
//! minimum: standard autonegotiation, nothing more. Linux and the BSD drivers
//! write per-revision values on top; which of them this chip needs for a
//! stable link from a cold power-on is a fact only the box can show
//! (`tools/net/rtl8168/`, plan section 5), not something to assume. A step is
//! added here only with the symptom it fixes written next to it and, if it is
//! a firmware patch, the licence decision recorded first.
//!
//! Each step is a read-modify-write of one standard-page MII register:
//! `(register, mask, value)`.

use crate::phy;
use crate::regs::Regs;
use crate::setup::SetupError;

/// A register update: bits in `mask` become `value`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub reg: u8,
    pub mask: u16,
    pub value: u16,
}

/// What the box has proven necessary. Empty: nothing is known to be needed.
pub const STEPS: &[Step] = &[];

/// Apply [`STEPS`] in order.
pub fn apply(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<(), SetupError> {
    apply_steps(regs, STEPS, &mut wait)
}

pub(crate) fn apply_steps(
    regs: &mut impl Regs,
    steps: &[Step],
    mut wait: impl FnMut(),
) -> Result<(), SetupError> {
    for step in steps {
        phy::update(regs, step.reg, step.mask, step.value, &mut wait)?;
    }
    Ok(())
}

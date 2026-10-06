//! The login console's keyboard claim (issue #396, `docs/input-plan.md` I5).
//!
//! `logind` reads its login prompt through `inputd`: a *sessionless* input
//! session, which `inputd` feeds while no compositor takes the keyboard.
//! Without more, every key would also land on the kernel terminal queue the
//! console shell reads later, so the shell would replay the user name and the
//! password as commands. The claim is the handoff: a task holding
//! `CAP_INPUT_CONSOLE` claims the console while it prompts, and while the
//! claim stands the keyboard driver keeps typed keys off the terminal queue
//! (`inputd` still reads them from the raw bus). `inputd` asks who holds it
//! ([`holder`]) and opens a sessionless session for that task only.
//!
//! The holder is a task slot. A claim is valid only while that slot is live
//! *and* still holds `CAP_INPUT_CONSOLE`, so a dead holder's slot reused by an
//! ordinary task never inherits it, and a crashed `logind` gives the
//! keyboard back to the terminal at once.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::ipc::credentials::{self, CAP_INPUT_CONSOLE};
use crate::task;

/// No holder.
const NONE: usize = usize::MAX;

/// The claiming task's slot, or [`NONE`]. Read from the keyboard path, so it
/// is an atomic rather than a lock.
static HOLDER: AtomicUsize = AtomicUsize::new(NONE);

/// Why a claim call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Another live task holds the console (`EBUSY`).
    Busy,
    /// The caller does not hold the claim it tried to release (`EPERM`).
    NotHolder,
}

/// Whether `slot` may hold the claim: live and still entitled.
fn valid(slot: usize) -> bool {
    slot != NONE && task::live(slot) && credentials::of(slot).has_cap(CAP_INPUT_CONSOLE)
}

/// [`valid`] for the keyboard path, which runs in the timer's interrupt and so
/// must not wait on the credential table: if a `credentials::set` the tick
/// interrupted holds it, the claim is taken to stand (a key kept off the
/// terminal queue for one more tick is recoverable, a spin there is not).
fn valid_in_irq(slot: usize) -> bool {
    slot != NONE
        && task::live(slot)
        && credentials::try_of(slot).is_none_or(|cred| cred.has_cap(CAP_INPUT_CONSOLE))
}

/// Claim the console for `me` (the syscall checked `CAP_INPUT_CONSOLE`).
/// Claiming again is not an error; a stale claim (dead or demoted holder) is
/// taken over.
pub fn claim(me: usize) -> Result<(), Error> {
    let mut current = HOLDER.load(Ordering::Acquire);
    loop {
        if current != me && valid(current) {
            return Err(Error::Busy);
        }
        match HOLDER.compare_exchange(current, me, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(()),
            Err(seen) => current = seen,
        }
    }
}

/// Give the console back; only its holder may.
pub fn release(me: usize) -> Result<(), Error> {
    HOLDER
        .compare_exchange(me, NONE, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| Error::NotHolder)
}

/// The task holding a valid claim, if any.
pub fn holder() -> Option<usize> {
    let slot = HOLDER.load(Ordering::Acquire);
    valid(slot).then_some(slot)
}

/// Whether typed keys must stay off the kernel terminal queue right now.
/// Called from the keyboard path, which can run in an interrupt.
pub fn claimed() -> bool {
    valid_in_irq(HOLDER.load(Ordering::Acquire))
}

/// Forget any claim (the test suite's reset between cases).
#[cfg(lazyos_tests)]
pub fn reset() {
    HOLDER.store(NONE, Ordering::Release);
}

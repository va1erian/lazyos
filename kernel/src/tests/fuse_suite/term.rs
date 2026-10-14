//! A daemon a fatal signal is about to end (docs/shutdown.md). A native task
//! acts on `SIGTERM` at its syscall return, so a daemon parked in `NEXT`
//! must leave the wait when one is pending, and must take no request on the
//! way out: only `SIGKILL` used to end that wait, and a daemon sent `SIGTERM`
//! (an orderly stop, or `mountd` ending a mount) slept to its deadline and
//! then took the request it died holding.

use super::*;
use crate::task::signal::{self, Disposition, SigInfo};

/// One `NEXT` of `owner` that would wait `ticks` for a request.
fn take(index: usize, owner: usize, ticks: u64) -> Result<Option<Request>, FuseError> {
    let deadline = task::ticks() + test_hook::offset() + ticks;
    fuse::next(index, owner, deadline, &mut |_| Ok(()))
}

fn raise(owner: usize, sig: u8) -> Result<(), String> {
    signal::send_to_slot(task::KERNEL_TASK, owner, sig, SigInfo::kernel())
        .map_err(|error| format!("signal {sig} to the daemon: {error:?}"))
}

/// Run `body(index, owner)` on a fuse slot whose daemon the fake does not
/// serve and whose parks are ticks of the fake clock.
fn on_idle_slot(body: impl FnOnce(usize, usize) -> Result<(), String>) -> Result<(), String> {
    let index = setup(Mode::Silent)?;
    test_hook::set_server(None);
    test_hook::set_parkless(true);
    let owner = with_fake(|fake| fake.owner);
    // Parked in `NEXT` the daemon's saved frame is a kernel frame; a user
    // frame with a fatal signal pending is ended by the scheduler's sweep,
    // which would turn a timer tick into a flake.
    task::harness::set_kernel_frame(owner).ok_or("the daemon has no frame")?;
    let result = body(index, owner);
    signal::harness::reset();
    teardown();
    result
}

/// `SIGTERM` ends the wait at once and takes nothing; a signal the task
/// ignores or blocks does not, and a blocked one does once unblocked.
pub fn term_ends_the_daemons_wait() -> Result<(), String> {
    on_idle_slot(|index, owner| {
        // Nothing pending: the wait runs to its deadline, in parked slices.
        check!(
            take(index, owner, 30) == Ok(None) && test_hook::parks() > 0,
            "an idle wait did not park (parks {})",
            test_hook::parks()
        );

        // Ignored: never pending, the wait is undisturbed.
        signal::set_action(owner, signal::SIGTERM, Disposition::Ignore)
            .map_err(|error| format!("ignore: {error:?}"))?;
        raise(owner, signal::SIGTERM)?;
        test_hook::set_parkless(true);
        check!(
            take(index, owner, 30) == Ok(None) && test_hook::parks() > 0,
            "an ignored SIGTERM ended the wait"
        );
        signal::set_action(owner, signal::SIGTERM, Disposition::Default)
            .map_err(|error| format!("default: {error:?}"))?;

        // Blocked: pending, but its default action cannot run yet.
        signal::set_blocked(owner, 1 << signal::SIGTERM);
        raise(owner, signal::SIGTERM)?;
        test_hook::set_parkless(true);
        check!(
            take(index, owner, 30) == Ok(None) && test_hook::parks() > 0,
            "a blocked SIGTERM ended the wait"
        );
        // Unblocked: the pending signal now ends it, with no park.
        signal::set_blocked(owner, 0);
        test_hook::set_parkless(true);
        let clock = test_hook::offset();
        check!(
            take(index, owner, 1000) == Ok(None)
                && test_hook::parks() == 0
                && test_hook::offset() == clock,
            "an unblocked SIGTERM did not end the wait: parks {}, {} ticks",
            test_hook::parks(),
            test_hook::offset() - clock
        );
        Ok(())
    })
}

/// A request already queued stays queued for a daemon that is ending, and a
/// daemon with nothing pending still takes it.
pub fn term_takes_no_request() -> Result<(), String> {
    on_idle_slot(|index, owner| {
        raise(owner, signal::SIGTERM)?;
        test_hook::hold_queued(index);
        check!(
            take(index, owner, 1000) == Ok(None),
            "a daemon with SIGTERM pending took a request"
        );
        check!(
            test_hook::queued(index),
            "the request left the queue with a dying daemon"
        );
        check!(test_hook::parks() == 0, "the dying daemon parked");

        signal::harness::clear_pending(owner);
        check!(
            matches!(take(index, owner, 1000), Ok(Some(_))),
            "a daemon with nothing pending did not take the request"
        );
        check!(!test_hook::queued(index), "the request was not taken");

        // A kill is the same (the daemon sits in the kernel, so the kill
        // wakes it rather than ending it).
        test_hook::hold_queued(index);
        raise(owner, signal::SIGKILL)?;
        check!(
            take(index, owner, 1000) == Ok(None) && test_hook::queued(index),
            "a killed daemon took a request"
        );
        Ok(())
    })
}

/// Soak: thousands of waits that meet a pending fatal signal, with and
/// without a queued request. None parks, none takes the request, the slot
/// stays usable for the next daemon, and the signal registry does not grow.
pub fn soak_term_unwinds_the_daemon() -> Result<(), String> {
    on_idle_slot(|index, owner| {
        let registry = signal::harness::registry_len();
        let sigs = [signal::SIGTERM, signal::SIGHUP, signal::SIGINT];
        for round in 0..3000usize {
            let sig = sigs[round % sigs.len()];
            test_hook::hold_queued(index);
            raise(owner, sig)?;
            test_hook::set_parkless(true);
            let clock = test_hook::offset();
            check!(
                take(index, owner, 1000) == Ok(None),
                "round {round}: signal {sig} did not end the wait"
            );
            check!(
                test_hook::parks() == 0 && test_hook::offset() == clock,
                "round {round}: the wait parked ({} parks)",
                test_hook::parks()
            );
            check!(
                test_hook::queued(index),
                "round {round}: the request was taken"
            );
            // The signal is consumed (as a delivery would), the daemon lives
            // on in this test and serves the request.
            signal::harness::clear_pending(owner);
            check!(
                matches!(take(index, owner, 1000), Ok(Some(_))),
                "round {round}: the slot did not serve its next daemon"
            );
        }
        let left = signal::harness::registry_len();
        check!(
            left <= registry + 1,
            "signal registry entries leaked: {registry} -> {left}"
        );
        crate::trace::test!("fuse_signal_soak_term:INFO:3000 waits ended by a pending signal");
        Ok(())
    })
}

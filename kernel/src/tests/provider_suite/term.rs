//! A provider a fatal signal is about to end (docs/shutdown.md): the same
//! rule as the fuse daemon's (`fuse_suite/term.rs`). A native task acts on
//! `SIGTERM` at its syscall return, so a provider parked in `NEXT` must leave
//! the wait when one is pending and take no request on the way out; only
//! `SIGKILL` used to end it.

use super::*;
use crate::task::signal::{self, Disposition, SigInfo};

/// One `NEXT` of `owner` that would wait `ticks` for a request.
fn take(
    disk: usize,
    owner: usize,
    ticks: u64,
) -> Result<Option<provider::Request>, provider::ProviderError> {
    let deadline = task::ticks() + test_clock::offset() + ticks;
    provider::next(disk, owner, deadline, &mut |_| Ok(()))
}

fn raise(owner: usize, sig: u8) -> Result<(), String> {
    signal::send_to_slot(task::KERNEL_TASK, owner, sig, SigInfo::kernel())
        .map_err(|error| format!("signal {sig} to the provider: {error:?}"))
}

/// Run `body(disk, owner)` on a disk its provider is not served by the fake
/// for, whose parks are ticks of the fake clock.
fn on_idle_disk(body: impl FnOnce(usize, usize) -> Result<(), String>) -> Result<(), String> {
    setup(Mode::Silent)?;
    test_clock::set_server(None);
    test_clock::set_parkless(true);
    let (disk, owner) = with_fake(|fake| (fake.disk, fake.owner));
    let result = body(disk, owner);
    signal::harness::reset();
    teardown();
    result
}

/// `SIGTERM` ends the wait at once; a signal the task ignores or blocks does
/// not, and a blocked one does once unblocked.
pub fn term_ends_the_providers_wait() -> Result<(), String> {
    on_idle_disk(|disk, owner| {
        check!(
            take(disk, owner, 30) == Ok(None) && test_clock::parks() > 0,
            "an idle wait did not park (parks {})",
            test_clock::parks()
        );

        signal::set_action(owner, signal::SIGTERM, Disposition::Ignore)
            .map_err(|error| format!("ignore: {error:?}"))?;
        raise(owner, signal::SIGTERM)?;
        test_clock::set_parkless(true);
        check!(
            take(disk, owner, 30) == Ok(None) && test_clock::parks() > 0,
            "an ignored SIGTERM ended the wait"
        );
        signal::set_action(owner, signal::SIGTERM, Disposition::Default)
            .map_err(|error| format!("default: {error:?}"))?;

        signal::set_blocked(owner, 1 << signal::SIGTERM);
        raise(owner, signal::SIGTERM)?;
        test_clock::set_parkless(true);
        check!(
            take(disk, owner, 30) == Ok(None) && test_clock::parks() > 0,
            "a blocked SIGTERM ended the wait"
        );
        signal::set_blocked(owner, 0);
        test_clock::set_parkless(true);
        let clock = test_clock::offset();
        check!(
            take(disk, owner, 1000) == Ok(None)
                && test_clock::parks() == 0
                && test_clock::offset() == clock,
            "an unblocked SIGTERM did not end the wait: parks {}, {} ticks",
            test_clock::parks(),
            test_clock::offset() - clock
        );
        Ok(())
    })
}

/// A request already queued stays queued for a provider that is ending, and
/// one with nothing pending still takes it.
pub fn term_takes_no_request() -> Result<(), String> {
    on_idle_disk(|disk, owner| {
        raise(owner, signal::SIGTERM)?;
        test_clock::hold_queued(disk);
        check!(
            take(disk, owner, 1000) == Ok(None),
            "a provider with SIGTERM pending took a request"
        );
        check!(
            test_clock::queued(disk),
            "the request left the queue with a dying provider"
        );
        check!(test_clock::parks() == 0, "the dying provider parked");

        signal::harness::clear_pending(owner);
        check!(
            matches!(take(disk, owner, 1000), Ok(Some(_))),
            "a provider with nothing pending did not take the request"
        );
        check!(!test_clock::queued(disk), "the request was not taken");

        // A kill is the same. The provider sits in the kernel (as one parked
        // in `NEXT` does), so the kill wakes it rather than ending it.
        task::harness::set_kernel_frame(owner).ok_or("the provider has no frame")?;
        test_clock::hold_queued(disk);
        raise(owner, signal::SIGKILL)?;
        check!(
            take(disk, owner, 1000) == Ok(None) && test_clock::queued(disk),
            "a killed provider took a request"
        );
        Ok(())
    })
}

/// Soak: thousands of waits that meet a pending fatal signal, with a queued
/// request. None parks, none takes the request, the disk serves the next
/// provider, and the signal registry does not grow.
pub fn soak_term_unwinds_the_provider() -> Result<(), String> {
    on_idle_disk(|disk, owner| {
        let registry = signal::harness::registry_len();
        let sigs = [signal::SIGTERM, signal::SIGHUP, signal::SIGINT];
        for round in 0..3000usize {
            let sig = sigs[round % sigs.len()];
            test_clock::hold_queued(disk);
            raise(owner, sig)?;
            test_clock::set_parkless(true);
            let clock = test_clock::offset();
            check!(
                take(disk, owner, 1000) == Ok(None),
                "round {round}: signal {sig} did not end the wait"
            );
            check!(
                test_clock::parks() == 0 && test_clock::offset() == clock,
                "round {round}: the wait parked ({} parks)",
                test_clock::parks()
            );
            check!(
                test_clock::queued(disk),
                "round {round}: the request was taken"
            );
            signal::harness::clear_pending(owner);
            check!(
                matches!(take(disk, owner, 1000), Ok(Some(_))),
                "round {round}: the disk did not serve its next provider"
            );
        }
        let left = signal::harness::registry_len();
        check!(
            left <= registry + 1,
            "signal registry entries leaked: {registry} -> {left}"
        );
        crate::trace::test!("provider_signal_soak_term:INFO:3000 waits ended by a pending signal");
        Ok(())
    })
}

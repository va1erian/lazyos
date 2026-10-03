//! Messenger events for a host with its own event loop: a LazyRAD form.
//!
//! A form cannot call `msg::run()` (the window would stop painting). Its
//! window instead calls [`Fabric::pump`] on a timer while the form has
//! sources ([`Fabric::active`]): every source the form registered is polled
//! without waiting (the kernel's poll deadline), and each ready event or
//! request runs its handler through the window's `call`, in the form's own
//! engine. When the window closes, [`Fabric::release`] unsubscribes and
//! unregisters what the form left behind.
//!
//! The work itself (`runloop::poll`, `runloop::handle`) is the same as
//! `msg::run`'s, so a service answers and a reliable topic is acked the same
//! way in both.

use alloc::boxed::Box;
use alloc::vec::Vec;

use rhai::EvalAltResult;

use super::bus::Wait;
use super::runloop::{handle, poll, Call, Source, SourceKind};
use super::service::Fabric;

/// Most pieces of work one source may deliver per pump, so a flood of events
/// cannot hold the window.
pub const PER_SOURCE: usize = 16;

/// What one [`Fabric::pump`] did.
#[derive(Default)]
pub struct Pumped {
    /// Events and requests handled.
    pub handled: usize,
    /// Errors to show the user: a topic handler that threw, a subscription
    /// the broker dropped. Only the first of a run of failures from one
    /// source is here; the rest are counted in `suppressed`.
    pub errors: Vec<Box<EvalAltResult>>,
    pub suppressed: usize,
}

impl Fabric {
    /// Whether `owner` registered anything a pump would poll.
    pub fn active(&self, owner: &str) -> bool {
        self.sources.borrow().iter().any(|s| s.owner == owner)
    }

    /// Run every handler of `owner` whose event or request is ready now,
    /// through `call`. Never waits.
    pub fn pump(&self, owner: &str, call: &mut Call) -> Pumped {
        let mut pumped = Pumped::default();
        // Sources a handler adds during this pass wait for the next one.
        let count = self.sources.borrow().len();
        for index in 0..count {
            if self.sources.borrow().get(index).map(|s| s.owner.as_str()) != Some(owner) {
                continue;
            }
            for _ in 0..PER_SOURCE {
                let outcome = match poll(self, index, Wait::Poll) {
                    Ok(Some(work)) => handle(self, work, call).map(|()| true),
                    Ok(None) => break,
                    Err(error) => Err(error),
                };
                match outcome {
                    Ok(_) => {
                        pumped.handled += 1;
                        self.reset_failures(index);
                    }
                    Err(error) => {
                        self.record_failure(index, error, &mut pumped);
                        break;
                    }
                }
            }
        }
        pumped
    }

    fn reset_failures(&self, index: usize) {
        if let Some(source) = self.sources.borrow().get(index) {
            source.failures.set(0);
        }
    }

    fn record_failure(&self, index: usize, error: Box<EvalAltResult>, pumped: &mut Pumped) {
        let first = match self.sources.borrow().get(index) {
            Some(source) => {
                let failures = source.failures.get();
                source.failures.set(failures.saturating_add(1));
                failures == 0
            }
            None => true,
        };
        if first {
            pumped.errors.push(error);
        } else {
            pumped.suppressed += 1;
        }
    }

    /// Drop every source `owner` registered: subscriptions are closed on the
    /// broker and served names withdrawn. Best effort (the services may be
    /// gone); returns how many sources were dropped.
    pub fn release(&self, owner: &str) -> usize {
        let released: Vec<Source> = {
            let mut sources = self.sources.borrow_mut();
            let (gone, kept) = core::mem::take(&mut *sources)
                .into_iter()
                .partition(|s| s.owner == owner);
            *sources = kept;
            gone
        };
        for source in &released {
            match &source.kind {
                SourceKind::Topic { subscription, .. } => {
                    let _ = subscription.close();
                }
                SourceKind::Service { name, endpoint, .. } => {
                    let _ = self.bus().unregister(name, *endpoint);
                }
            }
        }
        released.len()
    }
}

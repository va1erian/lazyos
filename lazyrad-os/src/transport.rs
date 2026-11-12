//! One request/reply exchange with a Messenger service, and the real
//! transport over the kernel's Messenger calls.
//!
//! [`Transport`] is the seam [`crate::handoff`] is written against:
//! [`MessengerTransport`] talks to the real services, tests script a mock.

use std::collections::VecDeque;

use libmessenger::client::{CallError, Deadline, Errno};
use messenger_generated::os_lazy_pkgd_v1 as pkgd;
use xui_app::fabric::{pull, Feed, Watch};
use xui_app::platform::messenger::{instance_client, Conn};
use xui_app::sys::errno;

/// A refused or failed call: the service's own sentence, or the transport's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Positive errno-style code from a service, or the kernel's error negated
    /// for a transport failure.
    pub code: i64,
    /// Friendly text, ready to show.
    pub text: String,
}

impl Failure {
    /// Whether this is a poll that found nothing (`ETIMEDOUT`).
    pub fn is_timeout(&self) -> bool {
        self.code == -errno::ETIMEDOUT
    }
}

/// How long a call may wait for its reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Until the reply comes.
    Forever,
    /// Not at all: a pull with nothing queued fails with a timeout.
    Poll,
}

/// The package manager's events, from the moment they were followed.
pub trait PkgEvents {
    /// The next event, waiting no longer than the absolute kernel tick
    /// `until`: `None` when none came by then (or the follow is unusable).
    fn next(&mut self, until: u64) -> Option<pkgd::PkgEvent>;
}

/// One request/reply exchange with a Messenger service, and the clock the
/// handoff's waits run on.
pub trait Transport {
    /// Calls `method` of `interface` on the service registered as `service` and
    /// returns the reply body.
    fn call(
        &self,
        service: &'static str,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        wait: Wait,
    ) -> Result<Vec<u8>, Failure>;

    /// Follows `system/events/pkg/+`: what is published from now on (and the
    /// broker's retained replay) is what [`PkgEvents::next`] returns.
    fn follow_pkg_events(&self) -> Box<dyn PkgEvents + '_>;

    /// The kernel tick counter (100 Hz).
    fn ticks(&self) -> u64;

    /// Sleeps about `millis`.
    fn pause(&self, millis: u64);
}

/// The real transport: this thread's Messenger connection of `xui_app`
/// (`libmessenger::client::Connection` over the std transport), which
/// resolves once, keeps the handle, reconnects after a restart and reads the
/// structured error field, text included, from a refusal.
#[derive(Default)]
pub struct MessengerTransport;

impl Transport for MessengerTransport {
    fn call(
        &self,
        service: &'static str,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        wait: Wait,
    ) -> Result<Vec<u8>, Failure> {
        let until = match wait {
            Wait::Forever => Deadline::Never,
            Wait::Poll => Deadline::Poll,
        };
        // The handoff names its services by registry name and frames no request itself beyond
        // the generated stubs' bodies, so a family of one raw connection per name is the seam.
        instance_client("", service, std::convert::identity::<Conn>)
            .call(interface, method, body, Vec::new(), until)
            .map(|reply| reply.body)
            .map_err(|error| failure(service, error))
    }

    fn follow_pkg_events(&self) -> Box<dyn PkgEvents + '_> {
        Box::new(FollowEvents {
            feed: pkgd::subscribe_system_events_pkg(&mut Watch, "+").ok(),
            pending: VecDeque::new(),
        })
    }

    fn ticks(&self) -> u64 {
        xui_app::sys::clock_ticks()
    }

    fn pause(&self, millis: u64) {
        xui_app::sys::sleep_millis(millis);
    }
}

/// The real [`PkgEvents`]: a typed [`Feed`] of the process's watcher thread,
/// waited on through its doorbell (no timer, no broker call on this thread).
struct FollowEvents {
    feed: Option<Feed<pkgd::PkgEvent>>,
    pending: VecDeque<pkgd::PkgEvent>,
}

impl PkgEvents for FollowEvents {
    fn next(&mut self, until: u64) -> Option<pkgd::PkgEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            let feed = self.feed.as_mut()?;
            pull(feed, |event| self.pending.push_back(event.value));
            if !self.pending.is_empty() {
                continue;
            }
            if xui_app::sys::clock_ticks() >= until {
                return None;
            }
            match feed.handle().bell() {
                // Park until the watcher queues something or `until` passes.
                Some(bell) => {
                    let _ = xui_app::sys::msg_wait_any(&[bell], until);
                }
                None => xui_app::sys::sleep_millis(25),
            }
        }
    }
}

/// A failed call as the text and code the handoff shows.
fn failure(service: &str, error: CallError) -> Failure {
    match error {
        CallError::Remote { code, message, .. } => Failure {
            code: i64::from(code),
            text: message,
        },
        CallError::Transport(no) if no == Errno::NOENT => Failure {
            code: no.to_syscall(),
            text: format!("{service} is not running"),
        },
        other => Failure {
            code: other.code(),
            text: format!("{service} did not answer (error {})", other.code()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_the_services_own_sentence() {
        let refused = CallError::refusal(17, "that application is already installed");
        let failure = failure("os.lazy.pkgd", refused);
        assert_eq!(failure.code, 17);
        assert_eq!(failure.text, "that application is already installed");
    }

    #[test]
    fn a_missing_service_says_it_is_not_running() {
        let failure = failure("os.lazy.pkgd", CallError::Transport(Errno::NOENT));
        assert_eq!(failure.code, -2);
        assert_eq!(failure.text, "os.lazy.pkgd is not running");
    }

    #[test]
    fn only_a_negative_timeout_code_is_a_poll_that_found_nothing() {
        let timeout = Failure {
            code: -errno::ETIMEDOUT,
            text: String::new(),
        };
        assert!(timeout.is_timeout());
        assert!(!Failure {
            code: 110,
            ..timeout
        }
        .is_timeout());
    }
}

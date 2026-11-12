//! The events `init` learns from, read off the central broker.
//!
//! * `logind`'s login records (`system/events/login/session/<id>` and
//!   `.../end`): who owns each session, and a logout that ends its tasks
//!   (`sessions.rs`, `logout.rs`, issues #623, #651);
//! * `pkgd`'s provisioning progress (`system/events/pkg/provision`), for the
//!   autostart (`provisioning.rs`).
//!
//! Each is a [`central::Feed`]: the generated typed subscription, so nothing
//! here decodes a payload. Their doorbells are what the supervisor's loop
//! parks on beside its requests ([`Feeds::endpoints`]): an idle `init` does
//! not wake for them, and a record is read before the requests queued behind
//! it (the bell rings inside the publish, before its publisher can send
//! anything else). `messengerd` is a child of this supervisor, so the feeds
//! are made when it serves (the `Ready` that wakes the loop);
//! [`Feeds::next_retry`] is the one timed wake until then.
//!
//! The publisher of every record is checked against the running manifest row
//! it must be (the event carries the kernel-stamped task slot), as the
//! per-service broker did with the message sender.

use alloc::vec::Vec;

use user::central::Feed;
use user::messenger::pkgd::PkgEvent;
use user::messenger::{logind, pkgd, Endpoint};

use super::service::Service;

/// Ticks (100 Hz) the last pull of [`Feeds::catch_up`] waits for a record.
const CATCH_UP_TICKS: u64 = 3;

/// The three feeds and the bits their doorbells have in the loop's wait mask.
pub(super) struct Feeds {
    session: Feed<logind::wire::LoginSession>,
    end: Feed<logind::wire::LoginEnd>,
    provision: Feed<PkgEvent>,
    bits: [u64; 3],
}

impl Feeds {
    pub(super) const fn new() -> Feeds {
        Feeds {
            session: Feed::new(|bus| logind::wire::subscribe_system_events_login_session(bus, "+")),
            end: Feed::new(logind::wire::subscribe_system_events_login_end),
            provision: Feed::new(|bus| pkgd::wire::subscribe_system_events_pkg(bus, "provision")),
            bits: [0; 3],
        }
    }

    /// The tick the loop must wake at to make the feeds still missing.
    pub(super) fn next_retry(&self) -> Option<u64> {
        [
            self.session.next_retry(),
            self.end.next_retry(),
            self.provision.next_retry(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Make the feeds that are missing, once their time has come.
    pub(super) fn ensure(&mut self) {
        self.session.ensure();
        self.end.ensure();
        self.provision.ensure();
    }

    /// The doorbells to park on beside the loop's own endpoints, which take
    /// the first `first_bit` bits of the wait mask.
    pub(super) fn endpoints(&mut self, first_bit: u32) -> Vec<Endpoint> {
        let bells = [self.session.bell(), self.end.bell(), self.provision.bell()];
        let mut parked = Vec::new();
        let mut bit = first_bit;
        for (slot, bell) in self.bits.iter_mut().zip(bells) {
            *slot = 0;
            if let Some(bell) = bell {
                *slot = 1 << bit;
                bit += 1;
                parked.push(bell);
            }
        }
        parked
    }

    /// Read the login feeds to their end now, the last pull waiting a few
    /// ticks: for a `Launch` into a session whose owner is not known yet.
    /// `logind` publishes the session record and then sends the `Launch`, but
    /// the broker rings its bell only after it answered the publisher, so the
    /// request can be here before the ring. Returns the sessions that ended.
    pub(super) fn catch_up(&mut self, services: &[Service]) -> Vec<u64> {
        let mut ended = Vec::new();
        self.session.drain_waiting(CATCH_UP_TICKS, |event| {
            super::sessions::observe_session(services, event.publisher, &event.topic, &event.value);
        });
        self.end.drain_waiting(CATCH_UP_TICKS, |event| {
            if let Some(session) =
                super::sessions::observe_end(services, event.publisher, &event.value)
            {
                ended.push(session);
            }
        });
        ended
    }

    /// [`Feeds::catch_up`] for the end feed alone, the last pull waiting one
    /// tick: for the login screen's `Launch`, which follows a logout's
    /// `login/end`.
    pub(super) fn catch_up_end(&mut self, services: &[Service]) -> Vec<u64> {
        let mut ended = Vec::new();
        self.end.drain_waiting(1, |event| {
            if let Some(session) =
                super::sessions::observe_end(services, event.publisher, &event.value)
            {
                ended.push(session);
            }
        });
        ended
    }

    /// Read what rang (`ready` is the wait's mask; `0` reads only the feeds
    /// just made; `now` reads the login feeds whatever rang, for a pass that
    /// is about to serve a request, see [`Feed::drain_now`]) and let each record's reader act on it. Returns the
    /// sessions that ended (a logout), whose tasks the caller must end.
    pub(super) fn drain(&mut self, services: &[Service], ready: u64) -> Vec<u64> {
        let mut ended = Vec::new();
        self.session.drain(ready & self.bits[0] != 0, |event| {
            super::sessions::observe_session(services, event.publisher, &event.topic, &event.value);
        });
        self.end.drain(ready & self.bits[1] != 0, |event| {
            if let Some(session) =
                super::sessions::observe_end(services, event.publisher, &event.value)
            {
                ended.push(session);
            }
        });
        self.provision.drain(ready & self.bits[2] != 0, |event| {
            super::provisioning::observe(services, event.publisher, &event.value);
        });
        ended
    }
}

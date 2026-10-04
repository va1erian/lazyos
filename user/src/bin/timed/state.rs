//! `timed`'s mutable state: the active zone, the `confd` link that keeps it
//! current, and the broker link the minute tick is published through.
//!
//! Every link is opened lazily and dropped on the first failure: `timed`
//! starts alongside `confd` and `messengerd`, so a link that is not up yet is
//! retried rather than treated as fatal, and a restarted peer is re-resolved.

use alloc::string::String;
use alloc::vec::Vec;

use timezone::{Local, Zone, DEFAULT_ZONE, ZONE_KEY};
use user::central::{Bus, Subscription};
use user::messenger::confd::{name_system_confd_changed, Client as Confd, CONFD_NOT_FOUND};
use user::messenger::{self, errno, timed as api, Endpoint, Error, EXPIRED_DEADLINE};
use user::sys;

/// Ticks (100 Hz) between attempts to reach `confd` while it is unreachable.
const RETRY_TICKS: u64 = 100;

pub(super) struct State {
    pub(super) zone: &'static Zone,
    /// Whether the zone was read from `confd` at least once.
    synced: bool,
    next_sync_try: u64,
    confd: Option<Confd>,
    watch: Option<Subscription>,
    /// The watch's doorbell: rung when a change notification is waiting.
    bell: Option<Endpoint>,
    bus: Option<Bus>,
    /// UTC second the next `time/tick` is due at.
    pub(super) next_tick: i64,
    /// Set once the first tick reached the broker (boot evidence).
    pub(super) announced: bool,
    /// Reused reply buffer for the watch poll (the heap never reclaims).
    buffer: Vec<u8>,
}

/// Current UTC seconds and the centisecond remainder.
pub(super) fn now() -> (i64, u32) {
    let centis = sys::wall_centis();
    ((centis / 100) as i64, (centis % 100) as u32)
}

impl State {
    pub(super) fn new() -> State {
        State {
            zone: timezone::default_zone(),
            synced: false,
            next_sync_try: 0,
            confd: None,
            watch: None,
            bell: None,
            bus: None,
            next_tick: 0,
            announced: false,
            buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// The zone's local-time parameters at `unix`.
    pub(super) fn local(&self, unix: i64) -> Local {
        timezone::local(self.zone, unix)
    }

    /// Read the zone from `confd` once, and (re)subscribe to its changes.
    /// Returns whether the active zone changed. Quiet when `confd` is not up.
    pub(super) fn sync_zone(&mut self) -> bool {
        if self.synced || sys::clock() < self.next_sync_try {
            return false;
        }
        match self.read_zone() {
            Ok(zone) => {
                self.drop_watch();
                let watched = self.confd.as_ref().and_then(|c| {
                    let topic = name_system_confd_changed(ZONE_KEY).ok()?;
                    let mut watch = c.watch(&topic).ok()?;
                    // The doorbell is what wakes this service for a change.
                    match watch.bell() {
                        Ok(bell) => Some((watch, bell)),
                        Err(_) => {
                            let _ = watch.unsubscribe();
                            None
                        }
                    }
                });
                if let Some((watch, bell)) = watched {
                    self.watch = Some(watch);
                    self.bell = Some(bell);
                    self.synced = true;
                    sys::write_str(
                        "TIMED:CONFD:SYNC watching
",
                    );
                } else {
                    // Without a subscription changes would go unnoticed: stay
                    // unsynced so the whole sync is retried shortly.
                    self.next_sync_try = sys::clock() + RETRY_TICKS;
                    sys::write_str(
                        "TIMED:CONFD:SYNC not watching
",
                    );
                }
                self.adopt(zone)
            }
            Err(_) => {
                self.confd = None;
                self.next_sync_try = sys::clock() + RETRY_TICKS;
                false
            }
        }
    }

    /// The watch's doorbell, to park on beside the service endpoint.
    pub(super) fn bell(&self) -> Option<Endpoint> {
        self.bell
    }

    /// The tick the service must wake at with nothing else to wake it: the
    /// next `time/tick`, or the next `confd` sync attempt.
    pub(super) fn next_wake(&self) -> Option<u64> {
        let (unix, centis) = now();
        let clock = sys::clock();
        let seconds = self.next_tick.saturating_sub(unix).max(0) as u64;
        let tick = clock + (seconds * 100).saturating_sub(u64::from(centis));
        let sync = (!self.synced).then_some(self.next_sync_try.max(clock));
        Some(sync.map_or(tick, |sync| sync.min(tick)))
    }

    /// Drain the `confd` change subscription when its bell `rung`; re-read
    /// the zone on any event. The drain ends with an empty pull, which
    /// re-arms the bell.
    pub(super) fn poll_changes(&mut self, rung: bool) -> bool {
        let Some(watch) = &self.watch else {
            return false;
        };
        if !rung {
            return false;
        }
        watch.take_ring(&mut self.buffer);
        let mut changed = false;
        let mut lost = false;
        loop {
            match watch.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(_)) => changed = true,
                Ok(None) => break,
                Err(_) => {
                    // The broker went away: resubscribe on the next sync.
                    lost = true;
                    break;
                }
            }
        }
        if lost {
            self.drop_watch();
            self.synced = false;
        }
        if !changed {
            return false;
        }
        match self.read_zone() {
            Ok(zone) => self.adopt(zone),
            Err(_) => {
                self.drop_watch();
                self.confd = None;
                self.synced = false;
                false
            }
        }
    }

    /// Release the change subscription at the broker, if any.
    fn drop_watch(&mut self) {
        self.bell = None;
        if let Some(watch) = self.watch.take() {
            let _ = watch.unsubscribe();
        }
    }

    /// Make `zone` active; returns whether it differs from the previous one.
    fn adopt(&mut self, zone: &'static Zone) -> bool {
        let changed = !core::ptr::eq(self.zone, zone);
        self.zone = zone;
        changed
    }

    /// The zone `confd` holds: absent means the default; a name outside the
    /// built-in table is ignored (default) rather than trusted.
    fn read_zone(&mut self) -> Result<&'static Zone, Error> {
        if self.confd.is_none() {
            self.confd = Some(Confd::connect()?);
        }
        let client = self.confd.as_ref().ok_or(Error::Errno(-errno::ENOENT))?;
        let name = match client.get(ZONE_KEY)? {
            Some(confd::Value::Str(name)) => name,
            Some(_) | None => String::from(DEFAULT_ZONE),
        };
        Ok(timezone::find(&name).unwrap_or_else(timezone::default_zone))
    }

    /// Persist `zone` to `confd` and make it active.
    pub(super) fn store_zone(&mut self, zone: &'static Zone) -> Result<(), Error> {
        if self.confd.is_none() {
            self.confd = Some(Confd::connect()?);
        }
        let client = self.confd.as_ref().ok_or(Error::Errno(-errno::ENOENT))?;
        let value = confd::Value::Str(String::from(zone.name));
        if let Err(error) = client.set(ZONE_KEY, &value) {
            self.confd = None;
            return Err(error);
        }
        self.synced = true;
        self.adopt(zone);
        Ok(())
    }

    /// Delete the stored zone (back to the default); demo/test helper.
    pub(super) fn clear_zone(&mut self) -> Result<(), Error> {
        let client = self.confd.as_ref().ok_or(Error::Errno(-errno::ENOENT))?;
        match client.delete(ZONE_KEY) {
            Err(Error::Confd(code)) if code == CONFD_NOT_FOUND => Ok(()),
            other => other,
        }
    }

    /// Whether the `confd` link is currently open (demo gating).
    pub(super) fn has_confd(&self) -> bool {
        self.confd.is_some() && self.synced
    }

    /// Publish the retained `time/tick` if it is due; schedules the next one.
    pub(super) fn publish_if_due(&mut self, unix: i64) {
        if unix < self.next_tick {
            return;
        }
        if self.publish_tick(unix).is_ok() {
            self.next_tick = timezone::next_tick_after(unix);
        } else {
            // Broker not up (or restarted): retry shortly, not every loop.
            self.next_tick = unix + 2;
        }
    }

    fn publish_tick(&mut self, unix: i64) -> Result<(), Error> {
        let local = self.local(unix);
        let value = api::wire::Tick {
            unix,
            offset: local.offset,
            zone_name: String::from(self.zone.name),
        };
        if self.bus.is_none() {
            self.bus = Some(Bus::connect()?);
        }
        let bus = self.bus.as_mut().ok_or(Error::Errno(-errno::ENOENT))?;
        match api::wire::publish_time_tick(bus, &value) {
            Ok(_) => {
                self.announced = true;
                Ok(())
            }
            Err(error) => {
                self.bus = None;
                Err(error)
            }
        }
    }
}

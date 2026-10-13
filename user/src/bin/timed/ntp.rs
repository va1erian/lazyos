//! The network time client (`docs/ntp-plan.md`): when the network has an
//! address, ask the configured servers for the time and step the wall clock
//! by the policy in `libs/sntp`.
//!
//! * Configuration is `confd` (`sys/time/ntp/enabled`, `sys/time/ntp/servers`,
//!   followed live) or the service argument `ntp.server=HOST[:PORT]`, which
//!   wins and switches the client on (the harness's server).
//! * The schedule, the clock-step policy and the packet rules are
//!   `sntp::{Schedule, decide, decode_reply}`: pure, and tested on the host.
//! * Boot evidence (serial, channel `TIMED`): `NTP:FLOOR`, `NTP:SYNC`,
//!   `NTP:STEP`, `NTP:REFUSE`, `NTP:FAIL`, `NTP:RATE`.

use alloc::string::String;
use alloc::vec::Vec;

use sntp::policy::{decide, Decision, Persistence, Refusal};
use sntp::schedule::Schedule;
use user::messenger::timed::wire;
use user::messenger::Endpoint;
use user::sys;
use user::trace;

use crate::ntp_exchange::{wall_unix_ns, Exchange, Finished};
use crate::ntp_net::NetWatch;
use crate::state::State;

/// Milliseconds between asking the stack for its address while there is
/// none (the topic is the normal wake; this covers a restarted `netd`).
const PROBE_MS: u64 = 30_000;

/// The last accepted exchange.
struct Last {
    server: String,
    stratum: u8,
    offset_ms: i64,
    delay_ms: i64,
    at_unix: i64,
}

pub(super) struct Ntp {
    /// `sys/time/ntp/enabled` as `confd` holds it (absent: on).
    configured_on: bool,
    /// `sys/time/ntp/servers` (absent: the default).
    configured_servers: String,
    /// The `ntp.server=` argument: forces the client on, to that server.
    forced: Option<String>,
    /// Whether `confd` was read at least once (until then it stays off, so a
    /// stored "off" is never raced).
    configured: bool,
    schedule: Schedule,
    net: NetWatch,
    exchange: Option<Exchange>,
    last: Option<Last>,
    failed: bool,
    next_probe_ms: u64,
    /// Addresses that told us not to ask again, for this boot.
    denied: Vec<[u8; 4]>,
    /// Agreeing refusals of one large offset (`Persistence`).
    persistence: Persistence,
}

/// Most denied addresses remembered (a pool hands out many; this is for a
/// misbehaving client, not a blocklist).
const DENIED_MAX: usize = 32;

fn jitter() -> u32 {
    let mut bytes = [0u8; 4];
    if sys::random(&mut bytes).is_err() {
        bytes = (sys::monotonic_ns() as u32).to_le_bytes();
    }
    u32::from_le_bytes(bytes)
}

/// The `ntp.server=` value of the service arguments.
fn forced_server() -> Option<String> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).ok()?;
    text.split_whitespace()
        .find_map(|part| part.strip_prefix("ntp.server="))
        .filter(|server| !server.is_empty())
        .map(String::from)
}

impl Ntp {
    pub(super) fn new() -> Ntp {
        Ntp {
            configured_on: true,
            configured_servers: String::from(sntp::DEFAULT_SERVER),
            forced: forced_server(),
            configured: false,
            schedule: Schedule::new(),
            net: NetWatch::new(),
            exchange: None,
            last: None,
            failed: false,
            next_probe_ms: 0,
            denied: Vec::new(),
            persistence: Persistence::new(),
        }
    }

    fn enabled(&self) -> bool {
        self.forced.is_some() || (self.configured && self.configured_on)
    }

    fn servers(&self) -> &str {
        self.forced.as_deref().unwrap_or(&self.configured_servers)
    }

    /// The network doorbell to park on beside the other endpoints.
    pub(super) fn bell(&self) -> Option<Endpoint> {
        self.net.bell()
    }

    /// The tick the service must wake at for the client: the next
    /// exchange's poll slice, a due query or the next address probe.
    pub(super) fn next_wake(&self) -> Option<u64> {
        if !self.enabled() {
            return None;
        }
        let now_ms = sys::monotonic_ms();
        let clock = sys::clock();
        let in_ms = |at_ms: u64| clock + at_ms.saturating_sub(now_ms).div_ceil(10);
        if self.exchange.is_some() {
            return Some(clock + 1);
        }
        match self.schedule.due_at() {
            Some(due) => Some(in_ms(due)),
            None => Some(in_ms(self.next_probe_ms)),
        }
    }

    /// Take a fresh configuration from `confd`.
    pub(super) fn configure(&mut self, on: bool, servers: Option<String>) {
        let was_on = self.enabled();
        let was_servers = String::from(self.servers());
        self.configured = true;
        self.configured_on = on;
        self.configured_servers = servers
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| String::from(sntp::DEFAULT_SERVER));
        if !self.enabled() {
            self.exchange = None;
            return;
        }
        // Switched on, or pointed somewhere else: ask now (the schedule's own
        // minimum interval and any RATE hold still apply), not at the next
        // hourly poll; and what an old server denied says nothing about the
        // new one.
        if !was_on || self.servers() != was_servers {
            if self.servers() != was_servers {
                self.denied.clear();
            }
            self.exchange = None;
            self.schedule.sync_now(sys::monotonic_ms());
        }
    }

    /// The wall clock was just stepped by someone else (`SetTime`): an
    /// exchange in flight measured the old clock and is dropped; the schedule
    /// asks again once the minimum interval has passed.
    pub(super) fn clock_stepped(&mut self) {
        self.exchange = None;
    }

    /// One pass of the client, from the service loop's housekeeping.
    pub(super) fn step(&mut self, state: &mut State, net_rung: bool) {
        if !self.enabled() {
            self.exchange = None;
            // Not just ignored: an unread doorbell in the wait set makes every
            // wait return at once.
            self.net.release();
            return;
        }
        let now_ms = sys::monotonic_ms();
        self.net.ensure();
        let events = self.net.drain(net_rung);
        let probe_due = !self.schedule.network() && now_ms >= self.next_probe_ms;
        if events || probe_due {
            self.next_probe_ms = now_ms + PROBE_MS;
            let up = self.net.has_address();
            if up.is_some() || events {
                self.schedule.set_network(up == Some(true), now_ms);
            }
        }
        if let Some(exchange) = &mut self.exchange {
            if let Some(done) = exchange.poll(now_ms) {
                self.exchange = None;
                self.finish(state, done, now_ms);
            }
            return;
        }
        if self.schedule.is_due(now_ms) {
            self.begin(now_ms);
        }
    }

    fn begin(&mut self, now_ms: u64) {
        self.schedule.attempted(now_ms);
        let servers = String::from(self.servers());
        let started = match self.net.clients() {
            Some((stack, sockets)) => Exchange::start(stack, sockets, &servers, &self.denied)
                .map_err(|failure| failure.text()),
            None => Err("no network stack"),
        };
        match started {
            Ok(exchange) => self.exchange = Some(exchange),
            Err(why) => {
                trace::line!(trace::Channel::Timed, "NTP:FAIL {why}");
                self.failed = true;
                self.schedule.failed(now_ms, jitter());
            }
        }
    }

    fn finish(&mut self, state: &mut State, done: Finished, now_ms: u64) {
        for ip in &done.denied {
            if self.denied.len() < DENIED_MAX && !self.denied.contains(ip) {
                self.denied.push(*ip);
                trace::line!(
                    trace::Channel::Timed,
                    "NTP:DENY {}.{}.{}.{} will not be asked again",
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3]
                );
            }
        }
        let Some(found) = done.best else {
            if done.rate_limited {
                trace::line!(trace::Channel::Timed, "NTP:RATE asked={}", done.asked);
                self.schedule.rate_limited(now_ms, jitter());
            } else {
                trace::line!(
                    trace::Channel::Timed,
                    "NTP:FAIL no usable answer asked={} answered={}",
                    done.asked,
                    done.answered
                );
                self.schedule.failed(now_ms, jitter());
            }
            self.failed = true;
            return;
        };
        let sample = found.sample;
        let now_ns = wall_unix_ns();
        let (offset_ms, delay_ms) = (sample.offset_ns / 1_000_000, sample.delay_ns / 1_000_000);
        let mut decision = decide(now_ns, sample.offset_ns, self.schedule.first_sync());
        if decision == Decision::Refuse(Refusal::TooLarge) {
            // One server saying so is not evidence; the same answer from
            // several rounds over ten minutes is a clock that really moved.
            if self.persistence.observe(now_ms, sample.offset_ns) {
                decision = decide(now_ns, sample.offset_ns, true);
            }
        } else {
            self.persistence.reset();
        }
        // Where "now" is on the clock `last_sync_unix` is read against.
        let mut at_ns = now_ns;
        match decision {
            Decision::Keep => trace::line!(
                trace::Channel::Timed,
                "NTP:SYNC offset_ms={offset_ms} delay_ms={delay_ms} stratum={} server={}",
                sample.stratum,
                found.label
            ),
            Decision::Step { target_unix_ns } => {
                let centis = (target_unix_ns / 10_000_000) as u64;
                match sys::wall_set_centis(centis) {
                    Ok(()) => {
                        // The retained `time/tick` must not show the old time.
                        state.next_tick = 0;
                        at_ns = target_unix_ns;
                        trace::line!(
                            trace::Channel::Timed,
                            "NTP:STEP offset_ms={offset_ms} delay_ms={delay_ms} stratum={} server={} unix={}",
                            sample.stratum,
                            found.label,
                            centis / 100
                        );
                    }
                    Err(code) => {
                        trace::line!(
                            trace::Channel::Timed,
                            "NTP:FAIL step refused errno={}",
                            -code
                        );
                        self.failed = true;
                        self.schedule.failed(now_ms, jitter());
                        return;
                    }
                }
            }
            Decision::Refuse(why) => {
                let reason = match why {
                    Refusal::BeforeFloor => "before the floor",
                    Refusal::BeyondRange => "beyond the clock's range",
                    Refusal::TooLarge => "too large a step after the first sync",
                };
                trace::line!(
                    trace::Channel::Timed,
                    "NTP:REFUSE {reason} offset_ms={offset_ms} server={}",
                    found.label
                );
                self.failed = true;
                self.schedule.failed(now_ms, jitter());
                return;
            }
        }
        self.failed = false;
        self.schedule.succeeded(now_ms, jitter());
        self.last = Some(Last {
            server: found.label,
            stratum: sample.stratum,
            offset_ms,
            delay_ms,
            at_unix: at_ns.div_euclid(1_000_000_000),
        });
    }

    /// The user's "sync now": `Err` is the errno to answer with.
    pub(super) fn sync_now(&mut self) -> Result<(), i64> {
        use user::messenger::errno;
        if !self.enabled() {
            return Err(errno::ENODEV);
        }
        if !self.schedule.network() {
            return Err(user::messenger::netsock::errno::ENETUNREACH);
        }
        if self.exchange.is_some() || !self.schedule.sync_now(sys::monotonic_ms()) {
            return Err(errno::EAGAIN);
        }
        Ok(())
    }

    /// The `NtpStatus` reply.
    pub(super) fn status(&self, now_unix: i64) -> wire::NtpStatusReply {
        let state = if !self.enabled() {
            wire::NTP_STATE_DISABLED
        } else if self.exchange.is_some() {
            wire::NTP_STATE_SYNCING
        } else if !self.schedule.network() {
            wire::NTP_STATE_WAITING_FOR_NETWORK
        } else if self.failed {
            wire::NTP_STATE_FAILED
        } else if self.schedule.first_sync() {
            wire::NTP_STATE_SYNCING
        } else {
            wire::NTP_STATE_SYNCHRONIZED
        };
        let next_sync_unix = match self.schedule.due_at() {
            Some(due) if self.enabled() => {
                let wait_s = due.saturating_sub(sys::monotonic_ms()) / 1000;
                now_unix.saturating_add(wait_s as i64)
            }
            _ => 0,
        };
        let last = self.last.as_ref();
        wire::NtpStatusReply {
            enabled: self.enabled(),
            state,
            server: last.map(|l| l.server.clone()).unwrap_or_default(),
            stratum: last.map_or(0, |l| u32::from(l.stratum)),
            offset_ms: last.map_or(0, |l| l.offset_ms),
            delay_ms: last.map_or(0, |l| l.delay_ms),
            last_sync_unix: last.map_or(0, |l| l.at_unix),
            next_sync_unix,
        }
    }
}

/// At service start: a clock earlier than the build's floor (a dead or reset
/// RTC) goes up to it before anything reads it. Needs no network.
pub(super) fn floor_clock(now_unix: i64) {
    let Some(floor) = sntp::floor_step(now_unix) else {
        return;
    };
    match sys::wall_set_centis(floor as u64 * 100) {
        Ok(()) => trace::line!(
            trace::Channel::Timed,
            "NTP:FLOOR from={now_unix} to={floor}"
        ),
        Err(code) => trace::line!(
            trace::Channel::Timed,
            "NTP:FLOOR:FAIL from={now_unix} errno={}",
            -code
        ),
    }
}

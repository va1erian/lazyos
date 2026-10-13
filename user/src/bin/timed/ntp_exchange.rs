//! One round of queries: resolve the configured servers, send each address a
//! request, and collect the replies while the service keeps serving.
//!
//! Nothing here blocks for long. Resolution parks for at most
//! [`RESOLVE_TIMEOUT_MS`] (the stack parks the call; documented in
//! `docs/ntp-plan.md` as the one place the service can stall), and replies
//! are polled in one-tick slices from the service loop, so a request for
//! `Now` waits at most a tick behind an exchange.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::num::NonZeroU64;

use sntp::packet::{decode_reply, encode_request, Kiss, Reject};
use sntp::sample::Sample;
use sntp::servers::{self, MAX_SERVERS};
use user::messenger::netsock::{self, Addr};
use user::messenger::netstack;
use user::messenger::netstd::{self, UdpSocket};
use user::sys;

/// How long one name lookup may take.
const RESOLVE_TIMEOUT_MS: u32 = 1500;
/// No further name is tried once this much of the service's time went on
/// lookups in one round: the call parks the whole service.
const RESOLVE_BUDGET_MS: u64 = 1500;
/// How long replies are awaited after the requests left.
const REPLY_WAIT_MS: u64 = 2000;
/// After the first usable reply, how much longer the others get.
const GRACE_MS: u64 = 300;
/// A reply is 48 bytes; room for an extension field or a MAC.
const RECV_MAX: usize = 128;
/// One poll's park (the stack's clock is the 10 ms tick).
const POLL_MS: u32 = 10;
/// Replies drained per poll.
const DRAIN_MAX: usize = 8;

/// Why an exchange could not even start.
#[derive(Clone, Copy, Debug)]
pub(super) enum Failure {
    /// No usable server entry in the configuration.
    NoServers,
    /// No name resolved to an address.
    NoAddress,
    /// The socket service refused a socket.
    Socket,
    /// Every send failed.
    Send,
    /// Every address asked (`DENY`, `RSTR`) not to be queried.
    Denied,
    /// No entropy for the nonce: without it a reply proves nothing.
    NoEntropy,
}

impl Failure {
    pub(super) fn text(self) -> &'static str {
        match self {
            Failure::NoServers => "no usable server in the configuration",
            Failure::NoAddress => "no server name resolved",
            Failure::Socket => "no socket",
            Failure::Send => "nothing could be sent",
            Failure::Denied => "every server address asked not to be queried",
            Failure::NoEntropy => "no random bytes for the request nonce",
        }
    }
}

struct Target {
    addr: Addr,
    host: String,
    nonce: NonZeroU64,
    /// The local wall clock when this request left (ns), and the monotonic
    /// clock then, which times the round trip.
    sent_unix_ns: i64,
    sent_mono_ns: u64,
    answered: bool,
}

/// An accepted sample and who it came from.
pub(super) struct Found {
    pub(super) sample: Sample,
    pub(super) label: String,
}

/// How a round ended.
pub(super) struct Finished {
    /// The accepted sample with the least delay, if any.
    pub(super) best: Option<Found>,
    /// A server answered `RATE`.
    pub(super) rate_limited: bool,
    pub(super) asked: usize,
    pub(super) answered: usize,
    /// Addresses that answered `DENY` or `RSTR`: not to be asked again.
    pub(super) denied: Vec<[u8; 4]>,
}

pub(super) struct Exchange {
    socket: UdpSocket,
    targets: Vec<Target>,
    found: Vec<Found>,
    deadline_ms: u64,
    grace_until_ms: Option<u64>,
    rate_limited: bool,
    denied: Vec<[u8; 4]>,
}

/// The local wall clock in Unix nanoseconds, taken at the middle of the tick
/// it was read in (the clock only advances every 10 ms).
pub(super) fn wall_unix_ns() -> i64 {
    sys::wall_centis() as i64 * 10_000_000 + 5_000_000
}

/// The request's nonce, the one thing an off-path sender cannot guess. No
/// fallback to a guessable value: without entropy the exchange is not sent.
fn random_nonce() -> Option<NonZeroU64> {
    let mut bytes = [0u8; 8];
    sys::random(&mut bytes).ok()?;
    NonZeroU64::new(u64::from_le_bytes(bytes))
}

/// How far the wall clock may disagree with the monotonic one over a round
/// trip before the clock is taken to have been stepped under the exchange
/// (its offset was measured against the old time). Lost timer ticks are
/// well inside it.
const STEP_SLACK_NS: i64 = 1_500_000_000;

impl Exchange {
    /// Resolve `servers` and send each address (but a `denied` one) a
    /// request. Names are tried in order until one gives addresses, and no
    /// new name after [`RESOLVE_BUDGET_MS`] of lookups.
    pub(super) fn start(
        stack: &netstack::Client,
        sockets: &Rc<netsock::Client>,
        servers: &str,
        denied: &[[u8; 4]],
    ) -> Result<Exchange, Failure> {
        let mut any = false;
        let mut skipped = false;
        let mut addrs: Vec<(Addr, String)> = Vec::new();
        let began = sys::monotonic_ms();
        for server in servers::parse(servers) {
            any = true;
            if !addrs.is_empty() || sys::monotonic_ms().saturating_sub(began) >= RESOLVE_BUDGET_MS {
                break;
            }
            let Ok(list) = stack.resolve(server.host, RESOLVE_TIMEOUT_MS) else {
                continue;
            };
            for ip in list {
                if denied.contains(&ip) {
                    skipped = true;
                    continue;
                }
                let addr = Addr::new(ip, server.port);
                if addrs.len() < MAX_SERVERS && !addrs.iter().any(|(a, _)| *a == addr) {
                    addrs.push((addr, String::from(server.host)));
                }
            }
        }
        if !any {
            return Err(Failure::NoServers);
        }
        if addrs.is_empty() {
            return Err(if skipped {
                Failure::Denied
            } else {
                Failure::NoAddress
            });
        }
        let socket = UdpSocket::bind(sockets, 0).map_err(|_| Failure::Socket)?;
        let wall_ns = wall_unix_ns();
        let mono_0 = sys::monotonic_ns();
        let mut targets = Vec::new();
        for (addr, host) in addrs {
            let Some(nonce) = random_nonce() else {
                return Err(Failure::NoEntropy);
            };
            let mono = sys::monotonic_ns();
            if socket.send_to(&encode_request(nonce), addr).is_err() {
                continue;
            }
            targets.push(Target {
                addr,
                host,
                nonce,
                sent_unix_ns: wall_ns + mono.saturating_sub(mono_0) as i64,
                sent_mono_ns: mono,
                answered: false,
            });
        }
        if targets.is_empty() {
            return Err(Failure::Send);
        }
        Ok(Exchange {
            socket,
            targets,
            found: Vec::new(),
            // From now, after the lookups: a slow resolver must not eat the
            // time the servers are given to answer.
            deadline_ms: sys::monotonic_ms() + REPLY_WAIT_MS,
            grace_until_ms: None,
            rate_limited: false,
            denied: Vec::new(),
        })
    }

    /// Take what has arrived (parking at most one tick when nothing has);
    /// `Some` once the round is over.
    pub(super) fn poll(&mut self, now_ms: u64) -> Option<Finished> {
        for _ in 0..DRAIN_MAX {
            match self.socket.recv_from(RECV_MAX, POLL_MS) {
                Ok((data, from)) => self.accept(&data, from, now_ms),
                Err(error) if netstd::is_timeout(&error) => break,
                // A dead socket will not answer: end at the deadline.
                Err(_) => {
                    self.deadline_ms = self.deadline_ms.min(now_ms);
                    break;
                }
            }
        }
        let all = self.targets.iter().all(|t| t.answered);
        let grace_over = self.grace_until_ms.is_some_and(|at| now_ms >= at);
        if !(all || grace_over || now_ms >= self.deadline_ms) {
            return None;
        }
        let mut found = core::mem::take(&mut self.found);
        found.sort_by_key(|f| f.sample.delay_ns);
        Some(Finished {
            best: found.into_iter().next(),
            rate_limited: self.rate_limited,
            asked: self.targets.len(),
            answered: self.targets.iter().filter(|t| t.answered).count(),
            denied: core::mem::take(&mut self.denied),
        })
    }

    fn accept(&mut self, data: &[u8], from: Addr, now_ms: u64) {
        let Some(target) = self
            .targets
            .iter_mut()
            .find(|t| t.addr == from && !t.answered)
        else {
            // Not from anyone asked, or a second answer: ignored.
            return;
        };
        match decode_reply(data, target.nonce) {
            Ok(reply) => {
                target.answered = true;
                let rtt = sys::monotonic_ns().saturating_sub(target.sent_mono_ns);
                // The wall clock moved by something other than time passing
                // since the request left: the sample is against a clock that
                // no longer exists, so it is not used.
                let wall_elapsed = wall_unix_ns().saturating_sub(target.sent_unix_ns);
                let stepped =
                    (i128::from(wall_elapsed) - i128::from(rtt)).abs() > i128::from(STEP_SLACK_NS);
                if stepped {
                    return;
                }
                if let Ok(sample) = Sample::measure(&reply, target.sent_unix_ns, rtt) {
                    let [a, b, c, d] = target.addr.ip;
                    self.found.push(Found {
                        sample,
                        label: format!("{} ({a}.{b}.{c}.{d})", target.host),
                    });
                    self.grace_until_ms.get_or_insert(now_ms + GRACE_MS);
                }
            }
            // Short, wrong version or mode, or not our nonce: not an answer
            // to this request (an off-path sender guessing), keep waiting.
            Err(Reject::Short | Reject::Version | Reject::Mode | Reject::Originate) => {}
            Err(Reject::Kiss(kiss)) => {
                target.answered = true;
                match kiss {
                    Kiss::Rate => self.rate_limited = true,
                    Kiss::Denied => self.denied.push(target.addr.ip),
                    Kiss::Other => {}
                }
            }
            // Ours, and the server says it is not fit to be believed.
            Err(_) => target.answered = true,
        }
    }
}

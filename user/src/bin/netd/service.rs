//! `os.lazy.net.stack.v1` (`idl/net.midl`): request dispatch for the stack.
//!
//! Nothing in a request names a caller: the kernel-stamped sender is the only
//! identity, used here for one thing, the per-caller cap on parked pings (the
//! ACL, not this code, decides who may call which method). A `Ping` is *parked*:
//! the transaction id is kept and the reply is sent when the stack reports the
//! result, so one thread serves any number of waiting callers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use netstack::config::Mode;
use netstack::{DhcpState, PingError, PingOutcome, PingResult, Source, Stack};
use user::central;
use user::messenger::netstack::{self as api, wire};
use user::messenger::{errno, services, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

use super::nic::Nic;

type Result<T> = core::result::Result<T, MsgError>;

/// The interface name clients see.
pub(super) const IFNAME: &str = "eth0";
/// Pings one caller may have waiting at once.
const PER_CALLER_PINGS: usize = 4;
/// `ENETUNREACH`: no address or no route yet.
const ENETUNREACH: i64 = 101;
/// `ENOSYS`: a declared method this build does not serve yet.
const ENOSYS: i64 = 38;

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

struct Parked {
    seq: u16,
    txn: u64,
    sender: u64,
}

pub(super) struct Netd {
    pub(super) stack: Stack,
    pub(super) nic: Nic,
    pub(super) mode: Mode,
    parked: Vec<Parked>,
    /// Times the NIC attachment was dropped and made again.
    pub(super) nic_resets: u64,
    /// A `Reattach` call asked for the attachment to be rebuilt.
    pub(super) reattach: bool,
    bus: Option<central::Bus>,
    published_epoch: u64,
    had_address: bool,
}

fn octets_or_zero(value: Option<[u8; 4]>) -> Vec<u8> {
    value.unwrap_or([0; 4]).to_vec()
}

fn network_of(addr: [u8; 4], prefix_len: u8) -> [u8; 4] {
    let mask = u32::MAX
        .checked_shl(32 - u32::from(prefix_len))
        .unwrap_or(0);
    (u32::from_be_bytes(addr) & mask).to_be_bytes()
}

fn text(addr: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
}

impl Netd {
    pub(super) fn new(stack: Stack, nic: Nic, mode: Mode) -> Netd {
        Netd {
            stack,
            nic,
            mode,
            parked: Vec::new(),
            nic_resets: 0,
            reattach: false,
            bus: None,
            published_epoch: 0,
            had_address: false,
        }
    }

    /// Route one request. `Ok(Some)` is the reply; `Ok(None)` means the call
    /// was parked and will be answered later; `Err` becomes the error reply.
    pub(super) fn dispatch(&mut self, message: &Message, now_ms: i64) -> Result<Option<Parcel>> {
        if message.interface_id() != api::INTERFACE {
            return Err(err(errno::EINVAL));
        }
        let method = message.method();
        let body = match method {
            wire::METHOD_INTERFACES => self.interfaces()?,
            wire::METHOD_ADDRESSES => self.addresses()?,
            wire::METHOD_ROUTES => self.routes()?,
            wire::METHOD_STATS => self.stats()?,
            wire::METHOD_PING => return self.ping(message, now_ms),
            // Declared in the N3 interface, served once the DNS half lands:
            // a distinct error, so a caller can tell "not yet" from a bad name.
            wire::METHOD_RESOLVE => return Err(err(ENOSYS)),
            wire::METHOD_RENEW => {
                self.stack.renew();
                Vec::new()
            }
            wire::METHOD_REATTACH => {
                self.reattach = true;
                Vec::new()
            }
            _ => return Err(err(errno::EINVAL)),
        };
        Ok(Some(api::parcel(method, body)))
    }

    fn interfaces(&self) -> Result<Vec<u8>> {
        let card = self.nic.card;
        let state = self.stack.state();
        wire::encode_interfaces_reply(&wire::InterfacesReply {
            list: alloc::vec![wire::InterfaceInfo {
                name: String::from(IFNAME),
                mac: self.stack.mac().to_vec(),
                mtu: card.map_or(0, |c| c.mtu),
                link: card.is_some_and(|c| c.link) && self.nic.attached(),
                mode: match self.mode {
                    Mode::Dhcp => wire::CONFIG_MODE_DHCP,
                    Mode::Static(_) => wire::CONFIG_MODE_STATIC,
                },
                dhcp: match state.dhcp {
                    DhcpState::Off => wire::DHCP_STATE_OFF,
                    DhcpState::Discovering => wire::DHCP_STATE_DISCOVERING,
                    DhcpState::Bound => wire::DHCP_STATE_BOUND,
                },
            }],
        })
        .map_err(MsgError::Parcel)
    }

    fn addresses(&self) -> Result<Vec<u8>> {
        let state = self.stack.state();
        let now_ms = sys::clock() as i64 * 10;
        let list = state
            .addr
            .map(|addr| wire::AddressInfo {
                interface: String::from(IFNAME),
                addr: addr.to_vec(),
                prefix_len: u32::from(state.prefix_len),
                source: match state.source {
                    Source::Dhcp => wire::ADDR_SOURCE_DHCP,
                    Source::Static => wire::ADDR_SOURCE_STATIC,
                },
                lease_secs: state
                    .lease_ends_ms
                    .map_or(0, |end| ((end - now_ms).max(0) / 1000) as u32),
            })
            .into_iter()
            .collect();
        wire::encode_addresses_reply(&wire::AddressesReply { list }).map_err(MsgError::Parcel)
    }

    fn routes(&self) -> Result<Vec<u8>> {
        let state = self.stack.state();
        let mut list = Vec::new();
        if let Some(addr) = state.addr {
            list.push(wire::RouteInfo {
                interface: String::from(IFNAME),
                dest: network_of(addr, state.prefix_len).to_vec(),
                prefix_len: u32::from(state.prefix_len),
                gateway: octets_or_zero(None),
            });
            if let Some(gateway) = state.gateway {
                list.push(wire::RouteInfo {
                    interface: String::from(IFNAME),
                    dest: octets_or_zero(None),
                    prefix_len: 0,
                    gateway: gateway.to_vec(),
                });
            }
        }
        wire::encode_routes_reply(&wire::RoutesReply { list }).map_err(MsgError::Parcel)
    }

    fn stats(&self) -> Result<Vec<u8>> {
        let d = self.stack.device_stats();
        let c = self.stack.counters();
        wire::encode_stats_reply(&wire::StatsReply {
            stats: wire::StackStats {
                rx_frames: d.rx_frames,
                tx_frames: d.tx_frames,
                rx_bytes: d.rx_bytes,
                tx_bytes: d.tx_bytes,
                tx_dropped: d.tx_dropped,
                rx_bad_length: d.rx_bad_length,
                nic_resets: self.nic_resets,
                leases: c.leases,
                lease_losses: c.lease_losses,
                pings_sent: c.pings_sent,
                pings_answered: c.pings_answered,
                pings_timed_out: c.pings_timed_out,
                // Name lookups arrive with the DNS half of N3; the fields
                // exist so the wire shape is final.
                lookups_sent: 0,
                lookups_answered: 0,
                lookups_failed: 0,
            },
        })
        .map_err(MsgError::Parcel)
    }

    /// Start a ping and park the call. Every argument is validated before
    /// anything is allocated or sent.
    fn ping(&mut self, message: &Message, now_ms: i64) -> Result<Option<Parcel>> {
        let args = wire::decode_ping_args(&message.parcel.body).map_err(MsgError::Parcel)?;
        // A reply needs a transaction; a one-way ping has nobody to answer.
        let Some(txn) = message.txn else {
            return Err(err(errno::EINVAL));
        };
        let dst: [u8; 4] = args
            .dst
            .as_slice()
            .try_into()
            .map_err(|_| err(errno::EINVAL))?;
        if args.payload_len > netstack::stack::MAX_PING_PAYLOAD as u32
            || !(10..=60_000).contains(&args.timeout_ms)
        {
            return Err(err(errno::EINVAL));
        }
        if self
            .parked
            .iter()
            .filter(|p| p.sender == message.sender)
            .count()
            >= PER_CALLER_PINGS
        {
            return Err(err(errno::EAGAIN));
        }
        let seq = self
            .stack
            .ping(
                dst,
                args.payload_len as usize,
                u64::from(args.timeout_ms),
                now_ms,
            )
            .map_err(|error| match error {
                PingError::BadArgument => err(errno::EINVAL),
                PingError::NoAddress | PingError::NoRoute => err(ENETUNREACH),
                PingError::Busy => err(errno::EAGAIN),
            })?;
        self.parked.push(Parked {
            seq,
            txn,
            sender: message.sender,
        });
        Ok(None)
    }

    /// Answer the parked pings the stack has finished with.
    pub(super) fn finish_pings(&mut self, server: &Endpoint) {
        for PingResult { seq, outcome } in self.stack.take_ping_results() {
            let Some(at) = self.parked.iter().position(|p| p.seq == seq) else {
                continue;
            };
            let parked = self.parked.remove(at);
            let reply = match outcome {
                PingOutcome::Reply {
                    rtt_ms,
                    source,
                    bytes,
                } => wire::encode_ping_reply(&wire::PingReply {
                    result: wire::EchoResult {
                        rtt_ms,
                        source: source.to_vec(),
                        bytes,
                    },
                })
                .map(|body| api::parcel(wire::METHOD_PING, body))
                .map_err(MsgError::Parcel),
                PingOutcome::TimedOut => Err(err(errno::ETIMEDOUT)),
            };
            let reply = reply.unwrap_or_else(|error| {
                services::error_reply(api::INTERFACE, wire::METHOD_PING, error)
            });
            // A caller that gave up or died has no transaction left: not a fault.
            let _ = server.reply_or_drop(parked.txn, &reply);
        }
    }

    /// Publish the address state if it changed, and announce the network the
    /// first time there is an address.
    pub(super) fn publish_if_changed(&mut self) {
        let epoch = self.stack.epoch();
        if epoch == self.published_epoch {
            return;
        }
        self.published_epoch = epoch;
        let state = self.stack.state();
        let event = wire::AddressEvent {
            interface: String::from(IFNAME),
            addr: octets_or_zero(state.addr),
            prefix_len: u32::from(state.prefix_len),
            gateway: octets_or_zero(state.gateway),
        };
        if let Some(addr) = state.addr {
            sys::write_str(&format!(
                "NETD:ADDR {}/{} gw={} dns={} source={}\n",
                text(addr),
                state.prefix_len,
                state.gateway.map_or(String::from("none"), text),
                state.dns.first().map_or(String::from("none"), |d| text(*d)),
                if state.source == Source::Dhcp {
                    "dhcp"
                } else {
                    "static"
                }
            ));
        } else {
            sys::write_str("NETD:ADDR none\n");
        }
        if self.bus.is_none() {
            self.bus = central::Bus::connect().ok();
        }
        let up = state.addr.is_some() && !self.had_address;
        self.had_address = state.addr.is_some();
        let Some(bus) = self.bus.as_mut() else { return };
        let _ = wire::publish_system_net_addr(bus, IFNAME, &event);
        if up {
            let _ = wire::publish_system_events_network_up(bus, &event);
        }
    }
}

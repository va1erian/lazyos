//! Typed client of the network stack service `netd` (`os.lazy.net.stack.v1`,
//! `idl/net.midl`) for the network apps.
//!
//! Calls go through the generated client, never hand-written field ids. Every
//! call runs on the UI thread, so each is bounded: a status call by the
//! `.midl` deadline, a ping or a lookup by its own timeout plus a margin. A
//! missing `netd` (an image without the stack, or one still booting) is one
//! failed resolve per call, reported as [`Error::NotRunning`], never a hang.
//! Replies are untrusted: addresses of the wrong length are dropped and names
//! are clipped before they reach a label.

use std::rc::Rc;

use libmessenger::client::{CallError, Deadline};
use messenger_generated::os_lazy_net_stack_v1 as wire;

use super::model::{self, Address, ConfigMode, DhcpState, Interface, NetStatus, Traffic};
use crate::format;
use crate::platform::messenger::{client, Observed};
use crate::sys::errno::{EACCES, EAGAIN, EINVAL, ENOENT, EPIPE, ETIMEDOUT};

/// Ticks a ping or lookup may run past its own timeout before the caller
/// gives up on `netd` itself.
const MARGIN_TICKS: u64 = 30;

/// `ENETUNREACH`: the stack's own code (it is not in the kernel's table).
const ENETUNREACH: i64 = 101;

/// Why a call to the stack failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `netd` is not registered.
    NotRunning,
    /// The call failed with this negative errno.
    Code(i64),
}

impl Error {
    /// The failure in words, for a status line.
    pub fn describe(self) -> String {
        match self {
            Error::NotRunning => String::from(
                "the network stack is not running (boot with networking: run_demo.py --net)",
            ),
            Error::Code(code) => match -code {
                ENOENT => String::from("no such name"),
                EAGAIN => String::from("busy, try again"),
                EACCES => String::from("not allowed"),
                EINVAL => String::from("invalid request"),
                EPIPE => String::from("the network stack is restarting, try again"),
                ENETUNREACH => String::from("no address or route yet"),
                ETIMEDOUT => String::from("timed out"),
                other => format!("error {other}"),
            },
        }
    }
}

/// `netd`'s client. A `netd` that was restarted (a crash, an update) leaves
/// this task's endpoint dead (`EPIPE`); the client resolves the new one and
/// retries once instead of failing a click.
fn netd() -> Result<Rc<wire::Client<Observed>>, Error> {
    client(wire::Client::try_connect).map_err(|_| Error::NotRunning)
}

fn failed(error: CallError) -> Error {
    Error::Code(error.code())
}

/// Read interfaces, addresses, routes and counters now, shown for the
/// interface called `selected` (the primary one, else the first, when `None`
/// or when that one is gone).
pub fn status(selected: Option<&str>) -> Result<NetStatus, Error> {
    let netd = netd()?;
    let list = netd.interfaces().map_err(failed)?;
    let names: Vec<String> = list.iter().map(|i| clipped(&i.name)).collect();
    let chosen = selected
        .and_then(|name| list.iter().position(|i| clipped(&i.name) == name))
        .or_else(|| list.iter().position(|i| i.primary))
        .or(if list.is_empty() { None } else { Some(0) });
    let chosen_name = chosen.map(|at| names[at].clone());
    let interface = chosen.map(|at| interface(&list[at]));
    let address = netd
        .addresses()
        .map_err(failed)?
        .iter()
        .filter(|a| Some(clipped(&a.interface)) == chosen_name)
        .find_map(address);
    let gateway = netd
        .routes()
        .map_err(failed)?
        .iter()
        .filter(|route| route.prefix_len == 0 && Some(clipped(&route.interface)) == chosen_name)
        .find_map(|route| model::ipv4(&route.gateway).filter(|gw| *gw != [0; 4]));
    let traffic = traffic(&netd.stats().map_err(failed)?);
    Ok(NetStatus {
        names,
        interface,
        address,
        gateway,
        traffic,
    })
}

/// An interface name from the wire, safe for a label.
fn clipped(name: &str) -> String {
    format::clip(&clean(name), 16)
}

fn interface(info: &wire::InterfaceInfo) -> Interface {
    Interface {
        name: clipped(&info.name),
        mac: model::mac(&info.mac),
        mtu: info.mtu,
        link: info.link,
        mode: match info.mode {
            wire::CONFIG_MODE_DHCP => ConfigMode::Dhcp,
            wire::CONFIG_MODE_STATIC => ConfigMode::Static,
            _ => ConfigMode::Unknown,
        },
        dhcp: match info.dhcp {
            wire::DHCP_STATE_OFF => DhcpState::Off,
            wire::DHCP_STATE_DISCOVERING => DhcpState::Discovering,
            wire::DHCP_STATE_BOUND => DhcpState::Bound,
            _ => DhcpState::Unknown,
        },
        wireless: info.kind == messenger_generated::os_lazy_net_nic_v1::NIC_KIND_WIRELESS,
        metric: info.metric,
        primary: info.primary,
    }
}

fn address(info: &wire::AddressInfo) -> Option<Address> {
    let addr = model::ipv4(&info.addr).filter(|a| *a != [0; 4])?;
    Some(Address {
        addr,
        prefix_len: info.prefix_len.min(32),
        from_dhcp: info.source == wire::ADDR_SOURCE_DHCP,
        lease_secs: info.lease_secs,
    })
}

fn traffic(stats: &wire::StackStats) -> Traffic {
    Traffic {
        rx_frames: stats.rx_frames,
        tx_frames: stats.tx_frames,
        rx_bytes: stats.rx_bytes,
        tx_bytes: stats.tx_bytes,
        pings_sent: stats.pings_sent,
        pings_answered: stats.pings_answered,
        lookups_sent: stats.lookups_sent,
        lookups_answered: stats.lookups_answered,
    }
}

/// One answered echo request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Echo {
    /// Round trip, milliseconds (10 ms resolution).
    pub rtt_ms: u32,
    pub from: [u8; 4],
    pub bytes: u32,
}

/// Ping `dst` once with `payload` bytes, waiting at most `timeout_ms`.
pub fn ping(dst: [u8; 4], payload: u32, timeout_ms: u32) -> Result<Echo, Error> {
    let until = Deadline::ticks(u64::from(timeout_ms / 10) + MARGIN_TICKS);
    let result = netd()?
        .ping_within(&dst, payload, timeout_ms, until)
        .map_err(failed)?;
    Ok(Echo {
        rtt_ms: result.rtt_ms,
        from: model::ipv4(&result.source).unwrap_or(dst),
        bytes: result.bytes,
    })
}

/// Look `name` up (A records), waiting at most `timeout_ms`. A dotted quad is
/// answered at once.
pub fn resolve(name: &str, timeout_ms: u32) -> Result<Vec<[u8; 4]>, Error> {
    let until = Deadline::ticks(u64::from(timeout_ms / 10) + MARGIN_TICKS);
    let addrs = netd()?
        .resolve_within(name, timeout_ms, until)
        .map_err(failed)?;
    Ok(addrs
        .iter()
        .filter_map(|addr| model::ipv4(addr))
        .take(8)
        .collect())
}

/// Ask for a fresh DHCP lease (the current one is dropped).
pub fn renew() -> Result<(), Error> {
    netd()?.renew().map_err(failed)
}

/// `text` without control characters, so a peer cannot break a label.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

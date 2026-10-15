//! Typed client of the Wi-Fi station service `wlanmd` (`os.lazy.net.wifi.v1`,
//! `idl/wifi.midl`) for the Network app's Wi-Fi page.
//!
//! Like [`super::stack`], bodies come from the generated stubs and every call
//! is bounded, because each runs on the UI thread. A missing `wlanmd` (an
//! image without Wi-Fi, or one still booting) is [`Error::NotRunning`], never
//! a hang. Replies are untrusted: they describe what any radio in range
//! sent, so names are cleaned and clipped again here before a label sees
//! them.
//!
//! The one secret that passes through is `add_network`'s passphrase. It is
//! sent as the call's argument and nowhere else: nothing is logged and no
//! reply carries one. The copies are dropped as soon as the call is made;
//! this does not claim to scrub memory (the toolkit's edit buffer and the
//! encoded request are not zeroized).

use messenger_generated::os_lazy_net_wifi_v1 as wire;

use messenger_generated::errors::ERROR_FIELD;

use super::wifimodel::{clean, Conn, Network, Security, State, Why, MAX_SSID};
use crate::platform::messenger::Service;

/// `wlanmd`'s registered name.
const NAME: &str = "os.lazy.net.wifi";
/// The longest one call may wait (PIT ticks, 100 Hz).
pub const CALL_TICKS: u64 = 100;
/// Most networks and interfaces taken from one reply (the service caps the
/// list at 128).
const MAX_NETWORKS: usize = 128;
const MAX_INTERFACES: usize = 8;
/// Longest interface name or profile id kept.
const MAX_NAME: usize = 24;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EAGAIN: i64 = 11;
const EACCES: i64 = 13;
const EBUSY: i64 = 16;
/// `EEXIST`: the network is saved already.
pub const EEXIST: i64 = 17;
const ENODEV: i64 = 19;
const EINVAL: i64 = 22;
const ENOSPC: i64 = 28;
const EPIPE: i64 = 32;

/// Why a call to `wlanmd` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `wlanmd` is not registered.
    NotRunning,
    /// The call failed with this negative errno.
    Code(i64),
}

impl Error {
    /// The failure in words, for the page's message line.
    pub fn describe(self) -> String {
        match self {
            Error::NotRunning => String::from(
                "the Wi-Fi service is not running (boot with Wi-Fi: run_demo.py --wifi or --wifi-sim)",
            ),
            Error::Code(code) => match -code {
                EPERM => String::from("not allowed for this account"),
                ENOENT => String::from("no such network"),
                EAGAIN => String::from("too many requests, try again in a moment"),
                EACCES => String::from("not allowed"),
                EBUSY => String::from("busy, try again"),
                EEXIST => String::from("that network is already saved"),
                ENODEV => String::from("no such Wi-Fi interface"),
                EINVAL => String::from(
                    "refused: check the network name and passphrase (8 to 63 characters)",
                ),
                ENOSPC => String::from("too many saved networks, forget one first"),
                EPIPE => String::from("the Wi-Fi service is restarting, try again"),
                other => format!("error {other}"),
            },
        }
    }

    /// Whether the scan was only skipped because another is running or the
    /// last one was a second ago: not worth showing.
    pub fn is_busy(self) -> bool {
        matches!(self, Error::Code(code) if -code == EBUSY || -code == EAGAIN)
    }
}

fn call(method: u32, body: Vec<u8>) -> Result<Vec<u8>, Error> {
    let service = Service::try_connect(NAME).ok_or(Error::NotRunning)?;
    service
        .call(wire::INTERFACE_ID, method, ERROR_FIELD, body, CALL_TICKS)
        .map(|reply| reply.body)
        .map_err(Error::Code)
}

/// [`call`], once more when `wlanmd` was restarted under the cached endpoint
/// (`EPIPE`; the failed call evicted it). Not for a call that carries a
/// secret: its body is not kept to send twice.
fn call_retrying(method: u32, body: Vec<u8>) -> Result<Vec<u8>, Error> {
    match call(method, body.clone()) {
        Err(Error::Code(code)) if -code == EPIPE => call(method, body),
        other => other,
    }
}

/// A wire encode or decode failure is an invalid request, whichever way.
fn checked<T>(result: Result<T, impl Sized>) -> Result<T, Error> {
    result.map_err(|_| Error::Code(-EINVAL))
}

/// Whether `wlanmd` is registered, with or without a radio yet.
pub fn available() -> bool {
    Service::try_connect(NAME).is_some()
}

/// The wireless interfaces `wlanmd` manages (`wlan0`).
pub fn interfaces() -> Result<Vec<String>, Error> {
    let body = call_retrying(wire::METHOD_INTERFACES, Vec::new())?;
    Ok(checked(wire::decode_interfaces_reply(&body))?
        .names
        .iter()
        .take(MAX_INTERFACES)
        .map(|name| clean(name, MAX_NAME))
        .filter(|name| !name.is_empty())
        .collect())
}

/// The interface's connection state now.
pub fn status(ifname: &str) -> Result<State, Error> {
    let body = checked(wire::encode_status_args(&wire::StatusArgs {
        ifname: ifname.into(),
    }))?;
    let reply = call_retrying(wire::METHOD_STATUS, body)?;
    Ok(state(&checked(wire::decode_status_reply(&reply))?.status))
}

/// The networks heard on `ifname`, merged with the saved ones, strongest
/// first (the service's order).
pub fn networks(ifname: &str) -> Result<Vec<Network>, Error> {
    let body = checked(wire::encode_networks_args(&wire::NetworksArgs {
        ifname: ifname.into(),
    }))?;
    let reply = call_retrying(wire::METHOD_NETWORKS, body)?;
    Ok(checked(wire::decode_networks_reply(&reply))?
        .list
        .iter()
        .take(MAX_NETWORKS)
        .map(network)
        .collect())
}

/// Ask for a fresh scan, probing for `hidden` names too. Returns at once; the
/// results show in the next [`networks`].
pub fn scan(ifname: &str, hidden: &[String]) -> Result<(), Error> {
    let body = checked(wire::encode_scan_args(&wire::ScanArgs {
        ifname: ifname.into(),
        ssids: hidden.to_vec(),
    }))?;
    call_retrying(wire::METHOD_SCAN, body).map(|_| ())
}

/// Remember a network for the signed-in user and return its profile id.
/// `passphrase` is required for WPA2 and absent for an open network.
pub fn add_network(
    ssid: &str,
    security: Security,
    hidden: bool,
    passphrase: Option<String>,
) -> Result<String, Error> {
    let profile = wire::Profile {
        id: String::new(),
        ssid: ssid.into(),
        security: match security {
            Security::Open => wire::SECURITY_OPEN,
            Security::Wpa2Psk => wire::SECURITY_WPA2_PSK,
            Security::Unsupported => wire::SECURITY_UNSUPPORTED,
        },
        hidden,
        autoconnect: true,
        priority: 0,
        scope: wire::SCOPE_USER,
    };
    let args = wire::AddNetworkArgs {
        profile,
        passphrase,
    };
    let body = checked(wire::encode_add_network_args(&args))?;
    drop(args);
    let reply = call(wire::METHOD_ADDNETWORK, body)?;
    Ok(clean(
        &checked(wire::decode_add_network_reply(&reply))?.id,
        MAX_NAME,
    ))
}

/// Join the saved network `id`. Returns when the attempt starts; the outcome
/// is [`status`].
pub fn connect(ifname: &str, id: &str) -> Result<(), Error> {
    let body = checked(wire::encode_connect_args(&wire::ConnectArgs {
        ifname: ifname.into(),
        id: id.into(),
    }))?;
    call_retrying(wire::METHOD_CONNECT, body).map(|_| ())
}

/// Leave the network and stop reconnecting until asked.
pub fn disconnect(ifname: &str) -> Result<(), Error> {
    let body = checked(wire::encode_disconnect_args(&wire::DisconnectArgs {
        ifname: ifname.into(),
    }))?;
    call_retrying(wire::METHOD_DISCONNECT, body).map(|_| ())
}

/// Delete a saved network and its passphrase.
pub fn forget(id: &str) -> Result<(), Error> {
    let body = checked(wire::encode_forget_args(&wire::ForgetArgs {
        id: id.into(),
    }))?;
    call_retrying(wire::METHOD_FORGET, body).map(|_| ())
}

fn security(wire_value: u32) -> Security {
    match wire_value {
        wire::SECURITY_OPEN => Security::Open,
        wire::SECURITY_WPA2_PSK => Security::Wpa2Psk,
        _ => Security::Unsupported,
    }
}

fn network(info: &wire::Network) -> Network {
    Network {
        ssid: clean(&info.ssid, MAX_SSID),
        security: security(info.security),
        rssi_dbm: info.rssi_dbm.clamp(-127, 0),
        freq_mhz: info.freq_mhz,
        bss_count: info.bss_count,
        id: clean(&info.id, MAX_NAME),
        connected: info.connected,
    }
}

fn state(info: &wire::WifiState) -> State {
    State {
        conn: Conn::from_wire(info.state),
        ssid: clean(&info.ssid, MAX_SSID),
        rssi_dbm: info.rssi_dbm.clamp(-127, 0),
        freq_mhz: info.freq_mhz,
        id: clean(&info.id, MAX_NAME),
        why: match info.reason {
            wire::FAIL_REASON_NOT_FOUND => Why::NotFound,
            wire::FAIL_REASON_REJECTED => Why::Rejected,
            wire::FAIL_REASON_BAD_PASSPHRASE => Why::BadPassphrase,
            wire::FAIL_REASON_TIMEOUT => Why::Timeout,
            wire::FAIL_REASON_DEAUTHENTICATED => Why::Deauthenticated,
            wire::FAIL_REASON_BEACON_LOSS => Why::BeaconLoss,
            wire::FAIL_REASON_POLICY_MISMATCH => Why::PolicyMismatch,
            wire::FAIL_REASON_RADIO => Why::Radio,
            wire::FAIL_REASON_UNSUPPORTED => Why::Unsupported,
            _ => Why::None,
        },
    }
}

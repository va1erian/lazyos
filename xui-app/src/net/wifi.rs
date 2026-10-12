//! Typed client of the Wi-Fi station service `wlanmd`
//! (`os.lazy.net.wifi.v1`, `idl/wifi.midl`) for the desktop: the Network
//! app's Wi-Fi page, the Network Status tray applet and Settings.
//!
//! Bodies come from the generated stubs. Every call runs on the UI thread and
//! is bounded by [`CALL_TICKS`] (a connect or an add returns as soon as the
//! attempt is started; the outcome arrives on the state, read with
//! [`status`]). A missing `wlanmd` (an image without `LAZYOS_WIFI`, or one
//! still booting) is [`Error::NotRunning`], never a hang.
//!
//! Replies are untrusted: `wlanmd` sanitises SSIDs, but names are clipped
//! again here before they reach a label, and are only ever shown as text.
//!
//! **Secrets.** [`add_network`] is the one call that carries a passphrase;
//! the encoded request is wiped after the call, and the caller's copy is
//! taken by value and wiped too. Nothing here stores or logs it.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_net_wifi_v1 as wire;

use crate::platform::messenger::Service;

/// `wlanmd`'s registered name.
const NAME: &str = "os.lazy.net.wifi";
/// The longest one call may wait (PIT ticks, 100 Hz).
pub const CALL_TICKS: u64 = 300;
/// Longest SSID shown (the standard allows 32 octets; text may be longer).
const MAX_SSID_CHARS: usize = 64;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EAGAIN: i64 = 11;
const EACCES: i64 = 13;
const EEXIST: i64 = 17;
const ENODEV: i64 = 19;
const EINVAL: i64 = 22;
const ENOSPC: i64 = 28;
const EPIPE: i64 = 32;
const EBUSY: i64 = 16;
const ETIMEDOUT: i64 = 110;

/// Why a call to `wlanmd` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `wlanmd` is not registered.
    NotRunning,
    /// The call failed with this negative errno.
    Code(i64),
}

impl Error {
    /// The failure in words, for a status line.
    pub fn describe(self) -> String {
        match self {
            Error::NotRunning => {
                String::from("Wi-Fi is not available (an image built with LAZYOS_WIFI=1 runs it)")
            }
            Error::Code(code) => match -code {
                EPERM | EACCES => String::from("not allowed"),
                ENOENT => String::from("no such network"),
                EAGAIN => String::from("busy, try again in a moment"),
                EBUSY => String::from("a scan is already running"),
                EEXIST => String::from("that network is already saved"),
                ENODEV => String::from("no wireless interface"),
                EINVAL => String::from("invalid request"),
                ENOSPC => String::from("too many saved networks"),
                EPIPE => String::from("the Wi-Fi service is restarting, try again"),
                ETIMEDOUT => String::from("timed out"),
                other => format!("error {other}"),
            },
        }
    }
}

/// The connection state machine (`ConnState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disabled,
    Idle,
    Scanning,
    Authenticating,
    Associating,
    Handshake,
    Connected,
    Failed,
}

impl ConnState {
    pub fn from_wire(value: u32) -> ConnState {
        match value {
            wire::CONN_STATE_IDLE => ConnState::Idle,
            wire::CONN_STATE_SCANNING => ConnState::Scanning,
            wire::CONN_STATE_AUTHENTICATING => ConnState::Authenticating,
            wire::CONN_STATE_ASSOCIATING => ConnState::Associating,
            wire::CONN_STATE_HANDSHAKE => ConnState::Handshake,
            wire::CONN_STATE_CONNECTED => ConnState::Connected,
            wire::CONN_STATE_FAILED => ConnState::Failed,
            _ => ConnState::Disabled,
        }
    }

    /// Joining: any step between choosing a network and the open port.
    pub fn joining(self) -> bool {
        matches!(
            self,
            ConnState::Authenticating | ConnState::Associating | ConnState::Handshake
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ConnState::Disabled => "Unavailable",
            ConnState::Idle => "Not connected",
            ConnState::Scanning => "Searching",
            ConnState::Authenticating | ConnState::Associating => "Connecting",
            ConnState::Handshake => "Checking the password",
            ConnState::Connected => "Connected",
            ConnState::Failed => "Not connected",
        }
    }
}

/// Why the last attempt failed, in words (`FailReason`); `None` when it did not.
pub fn reason_text(reason: u32) -> Option<&'static str> {
    Some(match reason {
        wire::FAIL_REASON_NOT_FOUND => "the network is out of range",
        wire::FAIL_REASON_REJECTED => "the access point refused the connection",
        wire::FAIL_REASON_BAD_PASSPHRASE => "the password is probably wrong",
        wire::FAIL_REASON_TIMEOUT => "the access point stopped answering",
        wire::FAIL_REASON_DEAUTHENTICATED => "the access point ended the connection",
        wire::FAIL_REASON_BEACON_LOSS => "the signal was lost",
        wire::FAIL_REASON_POLICY_MISMATCH => "the network's security settings changed",
        wire::FAIL_REASON_RADIO => "the Wi-Fi card reported an error",
        wire::FAIL_REASON_UNSUPPORTED => {
            "the network's security mode is not supported (WPA2-AES only for now)"
        }
        _ => return None,
    })
}

/// Security of a network (`Security`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    Wpa2Psk,
    Unsupported,
}

impl Security {
    pub fn from_wire(value: u32) -> Security {
        match value {
            wire::SECURITY_OPEN => Security::Open,
            wire::SECURITY_WPA2_PSK => Security::Wpa2Psk,
            _ => Security::Unsupported,
        }
    }

    pub fn wire(self) -> u32 {
        match self {
            Security::Open => wire::SECURITY_OPEN,
            Security::Wpa2Psk => wire::SECURITY_WPA2_PSK,
            Security::Unsupported => wire::SECURITY_UNSUPPORTED,
        }
    }

    /// Whether joining needs a passphrase.
    pub fn locked(self) -> bool {
        self != Security::Open
    }
}

/// One heard network, merged with what the caller may know of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// Display text; empty for a hidden network.
    pub ssid: String,
    pub security: Security,
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    pub bss_count: u32,
    /// The saved profile's id when the caller may use it, else empty.
    pub id: String,
    pub connected: bool,
}

impl Network {
    /// Signal strength in bars, 0 to 4 ([`bars`]).
    pub fn bars(&self) -> u8 {
        bars(self.rssi_dbm)
    }
}

/// An interface's state now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: ConnState,
    pub ssid: String,
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    /// The profile in use, when the caller may see it.
    pub id: String,
    /// The `FailReason` ordinal (meaningful in `Failed`).
    pub reason: u32,
}

/// Signal strength in bars (0 to 4) from dBm, the usual desktop thresholds.
pub fn bars(rssi_dbm: i32) -> u8 {
    match rssi_dbm {
        0 => 0,
        r if r >= -55 => 4,
        r if r >= -67 => 3,
        r if r >= -75 => 2,
        r if r >= -85 => 1,
        _ => 0,
    }
}

/// The band of a frequency, for display.
pub fn band_label(freq_mhz: u32) -> &'static str {
    match freq_mhz {
        2400..=2500 => "2.4 GHz",
        4900..=5900 => "5 GHz",
        5925..=7125 => "6 GHz",
        _ => "",
    }
}

fn clipped(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_SSID_CHARS)
        .collect()
}

/// One call on `wlanmd`; the reply body. A restarted `wlanmd` leaves the
/// cached endpoint dead (`EPIPE`): the failed call evicts it and one retry
/// reaches the new one.
fn call(method: u32, body: Vec<u8>) -> Result<Vec<u8>, Error> {
    match call_once(method, body.clone()) {
        Err(Error::Code(code)) if -code == EPIPE => call_once(method, body),
        other => other,
    }
}

fn call_once(method: u32, body: Vec<u8>) -> Result<Vec<u8>, Error> {
    let service = Service::try_connect(NAME).ok_or(Error::NotRunning)?;
    service
        .call_within(wire::INTERFACE_ID, method, ERROR_FIELD, body, CALL_TICKS)
        .map(|reply| reply.body)
        .map_err(Error::Code)
}

fn decoded<T, E>(result: Result<T, E>) -> Result<T, Error> {
    result.map_err(|_| Error::Code(-EINVAL))
}

/// The wireless interfaces (`wlan0`, ...).
pub fn interfaces() -> Result<Vec<String>, Error> {
    let body = call(wire::METHOD_INTERFACES, Vec::new())?;
    Ok(decoded(wire::decode_interfaces_reply(&body))?
        .names
        .iter()
        .map(|n| clipped(n))
        .collect())
}

/// Start a scan of `ifname` (results come later through [`networks`]).
pub fn scan(ifname: &str) -> Result<(), Error> {
    let args = wire::ScanArgs {
        ifname: String::from(ifname),
        ssids: Vec::new(),
    };
    call(wire::METHOD_SCAN, decoded(wire::encode_scan_args(&args))?).map(drop)
}

/// The networks heard on `ifname`, strongest first.
pub fn networks(ifname: &str) -> Result<Vec<Network>, Error> {
    let args = wire::NetworksArgs {
        ifname: String::from(ifname),
    };
    let body = call(
        wire::METHOD_NETWORKS,
        decoded(wire::encode_networks_args(&args))?,
    )?;
    let mut list: Vec<Network> = decoded(wire::decode_networks_reply(&body))?
        .list
        .iter()
        .map(|n| Network {
            ssid: clipped(&n.ssid),
            security: Security::from_wire(n.security),
            rssi_dbm: n.rssi_dbm,
            freq_mhz: n.freq_mhz,
            bss_count: n.bss_count,
            id: clipped(&n.id),
            connected: n.connected,
        })
        .collect();
    list.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.rssi_dbm.cmp(&a.rssi_dbm))
    });
    Ok(list)
}

/// `ifname`'s state now.
pub fn status(ifname: &str) -> Result<Status, Error> {
    let args = wire::StatusArgs {
        ifname: String::from(ifname),
    };
    let body = call(
        wire::METHOD_STATUS,
        decoded(wire::encode_status_args(&args))?,
    )?;
    let s = decoded(wire::decode_status_reply(&body))?.status;
    Ok(Status {
        state: ConnState::from_wire(s.state),
        ssid: clipped(&s.ssid),
        rssi_dbm: s.rssi_dbm,
        freq_mhz: s.freq_mhz,
        id: clipped(&s.id),
        reason: s.reason,
    })
}

/// Save a network for the calling user (`Scope::User`) and return its id.
/// `passphrase` is required for WPA2 and absent for an open network; it is
/// wiped here after the call, as is the encoded request.
pub fn add_network(
    ssid: &str,
    security: Security,
    hidden: bool,
    passphrase: Option<String>,
) -> Result<String, Error> {
    let mut args = wire::AddNetworkArgs {
        profile: wire::Profile {
            id: String::new(),
            ssid: String::from(ssid),
            security: security.wire(),
            hidden,
            autoconnect: true,
            priority: 0,
            scope: wire::SCOPE_USER,
        },
        passphrase,
    };
    let encoded = wire::encode_add_network_args(&args);
    if let Some(secret) = args.passphrase.take() {
        wipe_string(secret);
    }
    let mut body = decoded(encoded)?;
    let result = call(wire::METHOD_ADDNETWORK, body.clone());
    body.fill(0);
    let reply = result?;
    Ok(clipped(
        &decoded(wire::decode_add_network_reply(&reply))?.id,
    ))
}

/// Join saved network `id` on `ifname`; returns once the attempt started.
pub fn connect(ifname: &str, id: &str) -> Result<(), Error> {
    let args = wire::ConnectArgs {
        ifname: String::from(ifname),
        id: String::from(id),
    };
    call(
        wire::METHOD_CONNECT,
        decoded(wire::encode_connect_args(&args))?,
    )
    .map(drop)
}

/// Leave the current network on `ifname` (auto-connect pauses until the
/// next connect).
pub fn disconnect(ifname: &str) -> Result<(), Error> {
    let args = wire::DisconnectArgs {
        ifname: String::from(ifname),
    };
    call(
        wire::METHOD_DISCONNECT,
        decoded(wire::encode_disconnect_args(&args))?,
    )
    .map(drop)
}

/// Delete saved network `id` (disconnects first if it is in use).
pub fn forget(id: &str) -> Result<(), Error> {
    let args = wire::ForgetArgs {
        id: String::from(id),
    };
    call(
        wire::METHOD_FORGET,
        decoded(wire::encode_forget_args(&args))?,
    )
    .map(drop)
}

/// Overwrite a secret's bytes before freeing it.
pub fn wipe_string(secret: String) {
    let mut bytes = secret.into_bytes();
    bytes.fill(0);
    core::hint::black_box(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_bars_follow_the_usual_thresholds() {
        assert_eq!(bars(-30), 4);
        assert_eq!(bars(-60), 3);
        assert_eq!(bars(-70), 2);
        assert_eq!(bars(-80), 1);
        assert_eq!(bars(-95), 0);
        assert_eq!(bars(0), 0, "no measurement");
    }

    #[test]
    fn hostile_text_is_clipped_and_stripped() {
        let s = clipped("home\u{7}\nnet");
        assert_eq!(s, "homenet");
        assert_eq!(clipped(&"x".repeat(500)).chars().count(), MAX_SSID_CHARS);
    }

    #[test]
    fn states_and_reasons_have_words() {
        assert_eq!(
            ConnState::from_wire(wire::CONN_STATE_CONNECTED),
            ConnState::Connected
        );
        assert_eq!(ConnState::from_wire(999), ConnState::Disabled);
        assert!(ConnState::Handshake.joining());
        assert!(reason_text(wire::FAIL_REASON_BAD_PASSPHRASE).is_some());
        assert_eq!(reason_text(wire::FAIL_REASON_NONE), None);
        assert_eq!(band_label(5500), "5 GHz");
    }
}

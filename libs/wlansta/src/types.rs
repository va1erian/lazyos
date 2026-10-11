//! The vocabulary of the station machine: what goes in ([`Event`]), what comes
//! out ([`Action`]), what it reports ([`Status`]) and what it joins
//! ([`Profile`]). Ordinals of the enums that cross a wire are the ones of
//! `idl/wifi.midl`, so the daemon maps them with `as u32`.

use alloc::string::String;
use alloc::vec::Vec;

use eapol::Key;

/// A MAC address.
pub type Mac = [u8; 6];

/// `os.lazy.net.wifi.hw.v1` `FrameKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// An 802.11 management frame, from the MAC header, without FCS.
    Mgmt = 0,
    /// An EAPOL frame as 802.3: destination, source, ethertype 0x888E, PDU.
    Eapol = 1,
}

impl FrameKind {
    pub fn from_wire(value: u32) -> Option<FrameKind> {
        match value {
            0 => Some(FrameKind::Mgmt),
            1 => Some(FrameKind::Eapol),
            _ => None,
        }
    }
}

/// `os.lazy.net.wifi.hw.v1` `Band`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Ghz2 = 0,
    Ghz5 = 1,
    Ghz6 = 2,
}

impl Band {
    pub fn from_wire(value: u32) -> Option<Band> {
        match value {
            0 => Some(Band::Ghz2),
            1 => Some(Band::Ghz5),
            2 => Some(Band::Ghz6),
            _ => None,
        }
    }

    /// The centre frequency of `channel` in this band, in MHz (0 when the
    /// channel does not exist in it).
    pub fn freq_mhz(self, channel: u8) -> u32 {
        let channel = u32::from(channel);
        match self {
            Band::Ghz2 if channel == 14 => 2484,
            Band::Ghz2 if (1..=13).contains(&channel) => 2407 + 5 * channel,
            Band::Ghz5 if channel >= 1 => 5000 + 5 * channel,
            Band::Ghz6 if (1..=233).contains(&channel) => 5950 + 5 * channel,
            _ => 0,
        }
    }
}

/// `os.lazy.net.wifi.hw.v1` `KeyKind` (the station installs no IGTK: no PMF).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Pairwise = 0,
    Group = 1,
}

/// `os.lazy.net.wifi.hw.v1` `StaState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaState {
    Idle = 0,
    Authenticated = 1,
    Associated = 2,
    Authorized = 3,
}

/// `os.lazy.net.wifi.v1` `ConnState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disabled = 0,
    Idle = 1,
    Scanning = 2,
    Authenticating = 3,
    Associating = 4,
    Handshake = 5,
    Connected = 6,
    Failed = 7,
}

impl ConnState {
    pub const fn name(self) -> &'static str {
        match self {
            ConnState::Disabled => "disabled",
            ConnState::Idle => "idle",
            ConnState::Scanning => "scanning",
            ConnState::Authenticating => "authenticating",
            ConnState::Associating => "associating",
            ConnState::Handshake => "handshake",
            ConnState::Connected => "connected",
            ConnState::Failed => "failed",
        }
    }

    pub fn from_wire(value: u32) -> Option<ConnState> {
        const ALL: [ConnState; 8] = [
            ConnState::Disabled,
            ConnState::Idle,
            ConnState::Scanning,
            ConnState::Authenticating,
            ConnState::Associating,
            ConnState::Handshake,
            ConnState::Connected,
            ConnState::Failed,
        ];
        ALL.get(value as usize).copied()
    }
}

/// `os.lazy.net.wifi.v1` `FailReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    None = 0,
    NotFound = 1,
    Rejected = 2,
    BadPassphrase = 3,
    Timeout = 4,
    Deauthenticated = 5,
    BeaconLoss = 6,
    PolicyMismatch = 7,
    Radio = 8,
}

impl FailReason {
    pub const fn name(self) -> &'static str {
        match self {
            FailReason::None => "none",
            FailReason::NotFound => "not-found",
            FailReason::Rejected => "rejected",
            FailReason::BadPassphrase => "bad-passphrase",
            FailReason::Timeout => "timeout",
            FailReason::Deauthenticated => "deauthenticated",
            FailReason::BeaconLoss => "beacon-loss",
            FailReason::PolicyMismatch => "policy-mismatch",
            FailReason::Radio => "radio",
        }
    }

    pub fn from_wire(value: u32) -> Option<FailReason> {
        const ALL: [FailReason; 9] = [
            FailReason::None,
            FailReason::NotFound,
            FailReason::Rejected,
            FailReason::BadPassphrase,
            FailReason::Timeout,
            FailReason::Deauthenticated,
            FailReason::BeaconLoss,
            FailReason::PolicyMismatch,
            FailReason::Radio,
        ];
        ALL.get(value as usize).copied()
    }
}

/// `os.lazy.net.wifi.v1` `Security`: what a network asks of a station.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Security {
    Open = 0,
    /// WPA2-Personal, CCMP-128, AKM 2 or 6, no management frame protection
    /// required: exactly what `eapol` supports.
    Wpa2Psk = 1,
    /// Anything else (WEP, WPA1/TKIP, SAE, 802.1X, PMF required, a malformed
    /// RSN element): heard and listed, never joined.
    Unsupported = 2,
}

impl Security {
    pub fn from_wire(value: u32) -> Option<Security> {
        match value {
            0 => Some(Security::Open),
            1 => Some(Security::Wpa2Psk),
            2 => Some(Security::Unsupported),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Security::Open => "open",
            Security::Wpa2Psk => "wpa2-psk",
            Security::Unsupported => "unsupported",
        }
    }
}

/// Who owns a profile (`os.lazy.net.wifi.v1` `Scope`, with the user's uid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// Everyone's, before login too; added through `elevd`.
    System,
    /// One user's.
    User(u32),
}

/// A known network: non-secret facts only (the passphrase is `keyd`'s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The service's id: a `confd` path segment and a `keyd` secret name.
    pub id: String,
    /// 1 to 32 octets.
    pub ssid: Vec<u8>,
    /// [`Security::Open`] or [`Security::Wpa2Psk`].
    pub security: Security,
    /// Probe for it by name: it may not show in beacons.
    pub hidden: bool,
    /// Join it on our own when it is in range.
    pub autoconnect: bool,
    /// Higher wins among several known networks in range.
    pub priority: u32,
    pub owner: Owner,
}

/// One heard frame from `ScanResults`, as the radio gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanEntry {
    /// The beacon or probe response, from the MAC header, without FCS.
    pub frame: Vec<u8>,
    pub channel: u32,
    /// A `Band` ordinal.
    pub band: u32,
    pub rssi_dbm: i32,
    pub age_ms: u32,
}

/// What happens to the radio, or what one of the machine's requests yielded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The radio accepted [`Action::Scan`] and named it.
    ScanStarted { scan_id: u32 },
    /// `ScanDone`, with every entry `ScanResults` gave for it.
    ScanDone {
        scan_id: u32,
        aborted: bool,
        entries: Vec<ScanEntry>,
    },
    /// `RxMgmt`: a frame from the joined BSS (hostile bytes).
    Rx {
        kind: FrameKind,
        rssi_dbm: i32,
        frame: Vec<u8>,
    },
    /// `BeaconLoss`.
    BeaconLoss,
    /// `Deauthenticated`: the AP ended the association.
    Deauthenticated { reason: u16 },
    /// A radio call made for an [`Action`] failed with this errno (negative).
    /// The caller stops running that batch of actions at the failure.
    HwError { op: HwOp, errno: i64 },
    /// Time passed (the caller's clock reached [`crate::Station::next_deadline`]).
    Tick,
}

/// Which radio call an [`Event::HwError`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwOp {
    Scan,
    AbortScan,
    Join,
    Tx,
    SetState,
    SetKey,
    Leave,
}

/// What the caller must do, in order. A radio call that fails stops the batch
/// and is reported back as [`Event::HwError`].
#[derive(Debug)]
pub enum Action {
    /// `Scan`: every channel, actively, probing for `ssids` (wildcard when
    /// empty). Report the id with [`Event::ScanStarted`].
    Scan { ssids: Vec<Vec<u8>> },
    /// `AbortScan`.
    AbortScan { scan_id: u32 },
    /// `Join` at 20 MHz.
    Join { bssid: Mac, channel: u8, band: Band },
    /// `TxMgmt`.
    Tx { kind: FrameKind, frame: Vec<u8> },
    /// `SetState`.
    SetState { state: StaState, aid: u16 },
    /// `SetKey` with cipher CCMP-128. The key wipes itself when dropped.
    SetKey {
        kind: KeyKind,
        index: u8,
        key: Key<16>,
        rsc: [u8; 8],
        addr: Mac,
    },
    /// `Leave` with an IEEE 802.11 reason code.
    Leave { reason: u16 },
    /// Ask `keyd` for the PMK of profile `id` (`WifiPmk`) and hand the
    /// answer to [`crate::Station::pmk`].
    FetchPmk {
        id: String,
        owner: Owner,
        ssid: Vec<u8>,
    },
}

/// Why `keyd` gave no PMK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmkError {
    /// No secret, or not a valid passphrase: joining cannot work.
    Missing,
    /// `keyd` did not answer: try again later.
    Unavailable,
}

/// The interface's state as `os.lazy.net.wifi.v1` `WifiState` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: ConnState,
    /// Sanitised ([`crate::ssid::display`]); empty when none.
    pub ssid: String,
    pub bssid: Option<Mac>,
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    /// The profile in use; empty when none.
    pub id: String,
    pub reason: FailReason,
    /// Changes of the fields above (not of `rssi_dbm`) since the start.
    pub changes: u32,
}

impl Status {
    pub(crate) fn idle() -> Status {
        Status {
            state: ConnState::Idle,
            ssid: String::new(),
            bssid: None,
            rssi_dbm: 0,
            freq_mhz: 0,
            id: String::new(),
            reason: FailReason::None,
            changes: 0,
        }
    }

    /// Same state, ignoring the signal and the counter.
    pub(crate) fn same(&self, other: &Status) -> bool {
        self.state == other.state
            && self.ssid == other.ssid
            && self.bssid == other.bssid
            && self.freq_mhz == other.freq_mhz
            && self.id == other.id
            && self.reason == other.reason
    }
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// `EBUSY`: a scan runs, or a join is between scan and handshake.
    Busy,
    /// `ENOENT`: no such profile.
    NoProfile,
    /// `EINVAL`: more probe SSIDs than allowed, or one over 32 octets.
    Invalid,
}

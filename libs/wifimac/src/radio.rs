//! The [`Radio`] trait: what a soft-MAC needs from the hardware below it.
//!
//! Two programs implement it: `wifid` over `libs/rtw88` (the RTL8822CE: PCIe
//! DMA rings, hardware CCMP keyed from a CAM, firmware for the rest) and
//! `wifisim` (802.11 frames inside Ethernet frames on a second virtio-net
//! card, CCMP in software). Everything above it, the frame formats, the
//! packet numbers, the scan, the association state and both Messenger
//! interfaces, is [`crate::SoftMac`]'s, so the two radios cannot drift.
//!
//! **Frames** cross this trait as 802.11 frames from the MAC header, without
//! FCS (the hardware adds and checks it).
//!
//! **Encryption.** On transmit, a frame with [`TxOptions::protected`] already
//! carries its CCMP header (the soft-MAC chose the packet number: the
//! RTL8822CE does not, `IEEE80211_KEY_FLAG_GENERATE_IV` in Linux terms) and
//! the plaintext after it; the radio encrypts that body with the key the
//! header's key id names (pairwise for an individually addressed receiver)
//! and appends the 8-octet MIC. On receive, the radio decrypts a protected
//! frame with its installed keys and hands it over **with the MAC header and
//! the CCMP header kept** ([`Received::decrypted`]), so the soft-MAC can check
//! the packet number for replays; a MIC the radio kept at the end is counted
//! in [`Received::trailer`]. A frame the radio could not decrypt (no key, MIC
//! failure) it drops, or passes with `decrypted` false, which the soft-MAC
//! drops.
//!
//! The radio never parses a body: the receive filter is by address
//! ([`Radio::set_bssid`], [`Radio::set_scan_filter`]), and a key is chosen
//! by the key id octet and the receiver address alone.

use alloc::vec::Vec;

use ieee80211::Mac;

/// Bands; ordinals are `os.lazy.net.wifi.hw.v1`'s `Band` (bit numbers of
/// [`RadioInfo::bands`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Ghz2 = 0,
    Ghz5 = 1,
    Ghz6 = 2,
}

impl Band {
    pub fn from_u32(value: u32) -> Option<Band> {
        match value {
            0 => Some(Band::Ghz2),
            1 => Some(Band::Ghz5),
            2 => Some(Band::Ghz6),
            _ => None,
        }
    }

    /// The band's bit in a band bitmap.
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// The band a channel number belongs to when none is named: 1-14 are
    /// 2.4 GHz, 32-177 are 5 GHz.
    pub fn of_channel(number: u32) -> Option<Band> {
        match number {
            1..=14 => Some(Band::Ghz2),
            32..=177 => Some(Band::Ghz5),
            _ => None,
        }
    }
}

/// Channel widths (`Width` ordinals).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Mhz20 = 0,
    Mhz40 = 1,
    Mhz80 = 2,
    Mhz160 = 3,
}

impl Width {
    pub fn from_u32(value: u32) -> Option<Width> {
        match value {
            0 => Some(Width::Mhz20),
            1 => Some(Width::Mhz40),
            2 => Some(Width::Mhz80),
            3 => Some(Width::Mhz160),
            _ => None,
        }
    }
}

/// A channel to tune to: its number within `band` and the width. Scanning
/// tunes 20 MHz; a join asks for the widest the client wants and the radio
/// may tune narrower.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    pub band: Band,
    pub number: u8,
    pub width: Width,
}

impl Channel {
    pub const fn new(band: Band, number: u8) -> Channel {
        Channel {
            band,
            number,
            width: Width::Mhz20,
        }
    }
}

/// Ciphers (`Cipher` ordinals, bit numbers of [`RadioInfo::ciphers`]).
pub const CIPHER_CCMP128: u32 = 0;
pub const CIPHER_GCMP256: u32 = 1;
pub const CIPHER_BIP_CMAC128: u32 = 2;

/// What the radio is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadioInfo {
    /// The station address (also the network card's).
    pub mac: Mac,
    /// A [`Band`] bitmap.
    pub bands: u32,
    /// A cipher bitmap (`CIPHER_*` bit numbers). The soft-MAC runs CCMP-128
    /// only and refuses keys of any other cipher.
    pub ciphers: u32,
    /// The largest frame [`Radio::transmit`] takes, from the MAC header to
    /// the end of the body, not counting the MIC the radio appends; also the
    /// largest management frame either way.
    pub max_frame: usize,
    /// Hardware key slots, pairwise and group together.
    pub key_slots: u32,
}

/// How a frame goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxOptions {
    /// Management or data: the hardware queue (management frames have their
    /// own) and whether the radio may aggregate.
    pub data: bool,
    /// The frame carries a CCMP header after the MAC header: encrypt what
    /// follows it and append the MIC (see the module notes).
    pub protected: bool,
    /// Send at the lowest basic rate of the band, as management frames,
    /// EAPOL and probes go, rather than the rate adaptation's choice.
    pub basic_rate: bool,
    /// The user priority (0-7, the TID) for the access category queue.
    pub priority: u8,
}

/// Key classes (`KeyKind` ordinals).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Pairwise = 0,
    Group = 1,
    Igtk = 2,
}

impl KeyKind {
    pub fn from_u32(value: u32) -> Option<KeyKind> {
        match value {
            0 => Some(KeyKind::Pairwise),
            1 => Some(KeyKind::Group),
            2 => Some(KeyKind::Igtk),
            _ => None,
        }
    }
}

/// A key to install in a hardware slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInstall<'a> {
    pub kind: KeyKind,
    /// The key id: 0 or 1 for a pairwise key, 1-3 for a group key.
    pub index: u8,
    /// A `CIPHER_*` value (always [`CIPHER_CCMP128`] today).
    pub cipher: u32,
    /// The temporal key, its cipher's exact length.
    pub key: &'a [u8],
    /// The receive sequence counter the handshake delivered. The soft-MAC
    /// checks replays itself; a radio that also does may seed its counter.
    pub rsc: u64,
    /// The peer: the BSSID (also for a group key).
    pub peer: Mac,
}

/// The association state the radio's firmware is told (media status, the
/// AID for power save). Ordinals are `StaState`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StaState {
    Idle = 0,
    Authenticated = 1,
    Associated = 2,
    Authorized = 3,
}

impl StaState {
    pub fn from_u32(value: u32) -> Option<StaState> {
        match value {
            0 => Some(StaState::Idle),
            1 => Some(StaState::Authenticated),
            2 => Some(StaState::Associated),
            3 => Some(StaState::Authorized),
            _ => None,
        }
    }
}

/// One received frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    /// From the MAC header, without FCS; a decrypted frame keeps its MAC
    /// header (Protected bit set) and CCMP header.
    pub frame: Vec<u8>,
    pub rssi_dbm: i8,
    /// The band and channel the radio was tuned to when it heard the frame.
    pub band: Band,
    pub channel: u8,
    /// The frame was protected and the radio decrypted it and verified its
    /// MIC.
    pub decrypted: bool,
    /// Octets at the end that are not payload (a MIC the radio kept).
    pub trailer: usize,
}

/// Why a radio operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioError {
    /// No room: the transmit queue or the key slots are full.
    Full,
    /// The radio cannot do this (a channel it lacks, a cipher it lacks, a
    /// frame too long).
    Invalid,
    /// The hardware failed (a firmware timeout, a lost device).
    Hardware,
}

/// The hardware below a [`crate::SoftMac`]. Calls are made from one thread;
/// none may block for long (a tune or a key write is a few register writes
/// or one firmware command).
pub trait Radio {
    /// The hardware facts; constant for the radio's life.
    fn info(&self) -> RadioInfo;
    /// Program the station address (once, at start).
    fn set_address(&mut self, mac: Mac) -> Result<(), RadioError>;
    /// Switch to `channel`. Frames heard afterwards report it.
    fn tune(&mut self, channel: Channel) -> Result<(), RadioError>;
    /// Filter reception to one BSS (`Some`) with our association id, or to
    /// none (`None`: only frames to our address and broadcast).
    fn set_bssid(&mut self, bssid: Option<Mac>, aid: u16) -> Result<(), RadioError>;
    /// While scanning, also receive beacons and probe responses of every
    /// BSS.
    fn set_scan_filter(&mut self, scanning: bool) -> Result<(), RadioError>;
    /// Tell the firmware the association state.
    fn set_state(&mut self, state: StaState) -> Result<(), RadioError>;
    /// Install (or atomically replace) a key.
    fn set_key(&mut self, key: &KeyInstall<'_>) -> Result<(), RadioError>;
    /// Remove a key; no error when the slot is empty.
    fn del_key(&mut self, kind: KeyKind, index: u8) -> Result<(), RadioError>;
    /// Queue one frame (see the module notes on protection). `Full` when the
    /// queue has no room: the frame was not taken.
    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), RadioError>;
    /// Whether [`Radio::transmit`] would take a frame now.
    fn tx_ready(&self) -> bool;
    /// The next received frame, if one is waiting.
    fn receive(&mut self) -> Option<Received>;
}

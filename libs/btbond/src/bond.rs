//! What is remembered about one bonded device.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use bthci::BdAddr;

use crate::Error;

pub const MAX_NAME: usize = 248;
pub const MAX_DESCRIPTOR: usize = 4096;
pub const MAX_REPORT_HANDLES: usize = 16;

/// `hid_flags` bits, from the SDP record (BR/EDR) or the HID information
/// characteristic (LE).
pub mod hid_flag {
    /// The device reconnects by paging the host (HIDReconnectInitiate).
    pub const RECONNECT_INITIATE: u8 = 1;
    /// The device waits to be paged (HIDNormallyConnectable).
    pub const NORMALLY_CONNECTABLE: u8 = 2;
    /// HIDBootDevice.
    pub const BOOT_DEVICE: u8 = 4;
    pub const ALL: u8 = 7;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrKind {
    BrEdr,
    LePublic,
    LeRandom,
}

impl AddrKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            AddrKind::BrEdr => 0,
            AddrKind::LePublic => 1,
            AddrKind::LeRandom => 2,
        }
    }

    pub(crate) fn from_code(c: u8) -> Result<AddrKind, Error> {
        match c {
            0 => Ok(AddrKind::BrEdr),
            1 => Ok(AddrKind::LePublic),
            2 => Ok(AddrKind::LeRandom),
            _ => Err(Error::Invalid("address kind")),
        }
    }

    pub fn is_le(self) -> bool {
        self != AddrKind::BrEdr
    }
}

/// Which input source class the bond registers (the class follows the bond's
/// record, so a mouse bond cannot present itself as a keyboard).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputClass {
    Keyboard,
    Pointer,
    Combo,
}

impl InputClass {
    pub(crate) fn code(self) -> u8 {
        match self {
            InputClass::Keyboard => 0,
            InputClass::Pointer => 1,
            InputClass::Combo => 2,
        }
    }

    pub(crate) fn from_code(c: u8) -> Result<InputClass, Error> {
        match c {
            0 => Ok(InputClass::Keyboard),
            1 => Ok(InputClass::Pointer),
            2 => Ok(InputClass::Combo),
            _ => Err(Error::Invalid("input class")),
        }
    }
}

/// A BR/EDR link key and its HCI key type (4 unauthenticated, 5
/// authenticated, 7 and 8 the P-256 forms).
#[derive(Clone, PartialEq, Eq)]
pub struct LinkKey {
    pub key: [u8; 16],
    pub key_type: u8,
}

/// An LE long term key with the EDIV and Rand it was issued under (zero for
/// Secure Connections).
#[derive(Clone, PartialEq, Eq)]
pub struct Ltk {
    pub key: [u8; 16],
    pub ediv: u16,
    pub rand: u64,
}

/// One HID report characteristic, so a reconnect needs no discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportHandle {
    pub value: u16,
    pub cccd: u16,
    pub report_id: u8,
    pub report_type: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gatt {
    pub report_map: u16,
    pub protocol_mode: u16,
    pub reports: Vec<ReportHandle>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Bond {
    pub addr: BdAddr,
    pub kind: AddrKind,
    /// The key was made with MITM protection (Passkey Entry or Numeric
    /// Comparison). Keyboards need this; a Just Works bond is a pointer.
    pub authenticated: bool,
    pub secure_connections: bool,
    /// Encryption key size in octets; 0 when no key is held.
    pub key_size: u8,
    pub link_key: Option<LinkKey>,
    pub ltk: Option<Ltk>,
    pub irk: Option<[u8; 16]>,
    pub class_of_device: u32,
    pub input: InputClass,
    pub hid_flags: u8,
    pub name: String,
    pub report_descriptor: Vec<u8>,
    pub gatt: Gatt,
}

impl Bond {
    /// Identity of a bond in a store.
    pub fn id(&self) -> (BdAddr, AddrKind) {
        (self.addr, self.kind)
    }

    /// The rules every stored or accepted bond satisfies.
    pub fn validate(&self) -> Result<(), Error> {
        let le = self.kind.is_le();
        if le && self.link_key.is_some() {
            return Err(Error::Invalid("link key on an LE bond"));
        }
        if !le && (self.ltk.is_some() || self.irk.is_some()) {
            return Err(Error::Invalid("LE key on a BR/EDR bond"));
        }
        if self.link_key.is_none() && self.ltk.is_none() {
            return Err(Error::Invalid("no key"));
        }
        if self.key_size != 0 && !(7..=16).contains(&self.key_size) {
            return Err(Error::Invalid("key size"));
        }
        if self.ltk.is_some() && self.key_size == 0 {
            return Err(Error::Invalid("key size"));
        }
        if self.class_of_device > 0x00FF_FFFF || self.hid_flags & !hid_flag::ALL != 0 {
            return Err(Error::Invalid("class or flags"));
        }
        if self.name.len() > MAX_NAME
            || self.report_descriptor.len() > MAX_DESCRIPTOR
            || self.gatt.reports.len() > MAX_REPORT_HANDLES
        {
            return Err(Error::TooLarge);
        }
        Ok(())
    }
}

/// Keys never print: a stray `{:?}` in a trace must not leak one.
impl fmt::Debug for Bond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bond")
            .field("addr", &self.addr)
            .field("kind", &self.kind)
            .field("authenticated", &self.authenticated)
            .field("input", &self.input)
            .field(
                "keys",
                &(
                    self.link_key.is_some(),
                    self.ltk.is_some(),
                    self.irk.is_some(),
                ),
            )
            .finish_non_exhaustive()
    }
}

//! Standard USB descriptors (USB 2.0 ch. 9.6) and the HID class descriptor
//! (HID 1.11 6.2.1), as far as `usbd` needs them: identify the device, then
//! list every interface of a configuration with its endpoints (any class:
//! `usbd` dispatches on the class code), and the HID view of the HID ones.
//!
//! A configuration descriptor is a chain of `(bLength, bDescriptorType, ...)`
//! records `wTotalLength` bytes long. The walk trusts nothing: a zero or
//! overlong `bLength` ends it with [`Error::BadLength`], a record of a known
//! type that is too short for its fields is refused the same way, and unknown
//! records are skipped by their length.

use crate::Error;

/// Descriptor types.
pub mod kind {
    pub const DEVICE: u8 = 1;
    pub const CONFIGURATION: u8 = 2;
    pub const STRING: u8 = 3;
    pub const INTERFACE: u8 = 4;
    pub const ENDPOINT: u8 = 5;
    pub const HID: u8 = 0x21;
    pub const REPORT: u8 = 0x22;
    pub const SS_ENDPOINT_COMPANION: u8 = 0x30;
}

/// Class codes (device or interface).
pub const CLASS_HID: u8 = 3;
pub const CLASS_MASS_STORAGE: u8 = 8;
pub const CLASS_HUB: u8 = 9;
/// `bInterfaceSubClass` of a HID interface that supports the boot protocol.
pub const SUBCLASS_BOOT: u8 = 1;

/// Sizes of the fixed descriptors.
pub const DEVICE_LEN: usize = 18;
pub const CONFIG_LEN: usize = 9;
const INTERFACE_LEN: usize = 9;
const ENDPOINT_LEN: usize = 7;
/// A HID descriptor with one class descriptor entry (the report descriptor).
const HID_LEN: usize = 9;
const SS_COMPANION_LEN: usize = 6;

/// Interfaces (alternate settings included) one configuration may report;
/// more are ignored.
pub const MAX_INTERFACES: usize = 16;
/// Endpoints kept per interface; more are ignored (HID has one or two, a
/// hub one, mass storage two or three).
pub const MAX_ENDPOINTS: usize = 4;

/// The boot protocol an interface declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    None,
    Keyboard,
    Mouse,
}

impl Protocol {
    fn from_byte(subclass: u8, protocol: u8) -> Protocol {
        match (subclass, protocol) {
            (SUBCLASS_BOOT, 1) => Protocol::Keyboard,
            (SUBCLASS_BOOT, 2) => Protocol::Mouse,
            _ => Protocol::None,
        }
    }
}

/// The device descriptor fields `usbd` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDescriptor {
    /// `bcdUSB`, e.g. `0x0200`.
    pub usb: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Endpoint 0 packet size (8, 16, 32 or 64; 9 meaning 512 on USB 3).
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub configurations: u8,
}

/// Endpoint transfer types (`bmAttributes` bits 0..=1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

/// An endpoint descriptor, with its SuperSpeed companion when one follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Endpoint {
    /// `bEndpointAddress`: number in the low nibble, bit 7 set for IN.
    pub address: u8,
    /// Bytes per packet (`wMaxPacketSize` bits 0..=10).
    pub max_packet: u16,
    /// `bInterval`, in the encoding of the device's speed.
    pub interval: u8,
    /// `bmAttributes`.
    pub attributes: u8,
    /// High-speed periodic: extra transactions per microframe
    /// (`wMaxPacketSize` bits 11..=12, 0..=2).
    pub extra: u8,
    /// SuperSpeed companion `bMaxBurst` (0..=15), 0 without one.
    pub max_burst: u8,
    /// SuperSpeed companion `wBytesPerInterval` (periodic), 0 without one.
    pub bytes_per_interval: u16,
}

impl Endpoint {
    /// The endpoint number (1..=15).
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn transfer(&self) -> Transfer {
        match self.attributes & 0x3 {
            0 => Transfer::Control,
            1 => Transfer::Isochronous,
            2 => Transfer::Bulk,
            _ => Transfer::Interrupt,
        }
    }
}

/// One interface descriptor (one alternate setting) and its endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// The HID descriptor's report descriptor length (HID interfaces only).
    pub report_len: u16,
    endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
}

impl Interface {
    /// Its endpoints, in descriptor order (endpoint 0 is never listed).
    pub fn endpoints(&self) -> impl Iterator<Item = &Endpoint> {
        self.endpoints.iter().flatten()
    }

    /// The first endpoint of `transfer` type in direction `is_in`.
    pub fn endpoint(&self, transfer: Transfer, is_in: bool) -> Option<Endpoint> {
        self.endpoints()
            .find(|e| e.transfer() == transfer && e.is_in() == is_in)
            .copied()
    }

    /// This interface as HID sees it, when it is a HID interface.
    pub fn hid(&self) -> Option<HidInterface> {
        (self.class == CLASS_HID).then(|| HidInterface {
            number: self.number,
            alternate: self.alternate,
            protocol: Protocol::from_byte(self.subclass, self.protocol),
            report_len: self.report_len,
            endpoint: self.endpoint(Transfer::Interrupt, true),
        })
    }
}

/// A HID interface and what `usbd` needs to drive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidInterface {
    pub number: u8,
    pub alternate: u8,
    pub protocol: Protocol,
    /// Length of the report descriptor (0 when the HID descriptor names none).
    pub report_len: u16,
    /// The first interrupt-IN endpoint, if the interface has one.
    pub endpoint: Option<Endpoint>,
}

/// One configuration: its value and every interface it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// `bConfigurationValue`, the argument of `SET_CONFIGURATION`.
    pub value: u8,
    /// `wTotalLength`.
    pub total_len: u16,
    interfaces: [Option<Interface>; MAX_INTERFACES],
}

impl Config {
    /// Every interface (each alternate setting once), in descriptor order.
    pub fn interfaces(&self) -> impl Iterator<Item = &Interface> {
        self.interfaces.iter().flatten()
    }

    /// Every HID interface found, in descriptor order.
    pub fn hid_interfaces(&self) -> impl Iterator<Item = HidInterface> + '_ {
        self.interfaces().filter_map(Interface::hid)
    }

    /// Every boot keyboard or mouse (alternate setting 0) with an
    /// interrupt-IN endpoint: a composite device (a gaming keyboard with a
    /// consumer-control interface, a receiver with a keyboard and a mouse)
    /// may have its boot interface anywhere in the list.
    pub fn boot_interfaces(&self) -> impl Iterator<Item = HidInterface> + '_ {
        self.hid_interfaces()
            .filter(|i| i.alternate == 0 && i.protocol != Protocol::None && i.endpoint.is_some())
    }

    /// The first boot keyboard or mouse ([`Config::boot_interfaces`]).
    pub fn first_boot(&self) -> Option<HidInterface> {
        self.boot_interfaces().next()
    }

    /// The first HID interface (alternate setting 0) with an interrupt-IN
    /// endpoint, boot-capable or not (a tablet has no boot protocol).
    pub fn first_hid(&self) -> Option<HidInterface> {
        self.hid_interfaces()
            .find(|i| i.alternate == 0 && i.endpoint.is_some())
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// Parse a device descriptor.
pub fn parse_device(bytes: &[u8]) -> Result<DeviceDescriptor, Error> {
    if bytes.len() < DEVICE_LEN {
        return Err(Error::Short);
    }
    if bytes[1] != kind::DEVICE {
        return Err(Error::WrongType);
    }
    if usize::from(bytes[0]) < DEVICE_LEN {
        return Err(Error::BadLength);
    }
    Ok(DeviceDescriptor {
        usb: u16_at(bytes, 2),
        class: bytes[4],
        subclass: bytes[5],
        protocol: bytes[6],
        max_packet0: bytes[7],
        vendor: u16_at(bytes, 8),
        product: u16_at(bytes, 10),
        configurations: bytes[17],
    })
}

/// `wTotalLength` of a configuration descriptor header, so the caller can
/// fetch the whole chain (it reads the 9-byte header first).
pub fn config_total_len(header: &[u8]) -> Result<u16, Error> {
    if header.len() < CONFIG_LEN {
        return Err(Error::Short);
    }
    if header[1] != kind::CONFIGURATION {
        return Err(Error::WrongType);
    }
    let total = u16_at(header, 2);
    if usize::from(header[0]) < CONFIG_LEN || usize::from(total) < CONFIG_LEN {
        return Err(Error::BadLength);
    }
    Ok(total)
}

/// Parse a whole configuration descriptor chain (`wTotalLength` bytes; any
/// bytes past it are ignored).
pub fn parse_config(bytes: &[u8]) -> Result<Config, Error> {
    let total = config_total_len(bytes)?;
    let chain = bytes.get(..usize::from(total)).ok_or(Error::Short)?;
    let mut config = Config {
        value: chain[5],
        total_len: total,
        interfaces: [None; MAX_INTERFACES],
    };
    let mut found = 0;
    // Index into `interfaces` that records attach to; `None` past the table.
    let mut current: Option<usize> = None;
    // The endpoint a SuperSpeed companion describes: (interface, slot).
    let mut last_endpoint: Option<(usize, usize)> = None;
    let mut at = usize::from(chain[0]);
    while at < chain.len() {
        let record = record_at(chain, at)?;
        match record[1] {
            kind::INTERFACE => {
                expect_len(record, INTERFACE_LEN)?;
                (current, last_endpoint) = (None, None);
                if found < MAX_INTERFACES {
                    config.interfaces[found] = Some(Interface {
                        number: record[2],
                        alternate: record[3],
                        class: record[5],
                        subclass: record[6],
                        protocol: record[7],
                        report_len: 0,
                        endpoints: [None; MAX_ENDPOINTS],
                    });
                    current = Some(found);
                    found += 1;
                }
            }
            kind::HID => {
                expect_len(record, HID_LEN)?;
                if let Some(interface) = current.and_then(|i| config.interfaces[i].as_mut()) {
                    if interface.class == CLASS_HID && record[6] == kind::REPORT {
                        interface.report_len = u16_at(record, 7);
                    }
                }
            }
            kind::ENDPOINT => {
                expect_len(record, ENDPOINT_LEN)?;
                last_endpoint = current.and_then(|i| add_endpoint(&mut config, i, record));
            }
            kind::SS_ENDPOINT_COMPANION => {
                expect_len(record, SS_COMPANION_LEN)?;
                let endpoint = last_endpoint
                    .take()
                    .and_then(|(i, e)| config.interfaces[i].as_mut()?.endpoints[e].as_mut());
                if let Some(endpoint) = endpoint {
                    endpoint.max_burst = record[2].min(15);
                    endpoint.bytes_per_interval = u16_at(record, 4);
                }
            }
            _ => {}
        }
        at += record.len();
    }
    Ok(config)
}

/// Record the endpoint descriptor `record` on interface `index`; returns
/// where it went, `None` for endpoint 0 or a full interface.
fn add_endpoint(config: &mut Config, index: usize, record: &[u8]) -> Option<(usize, usize)> {
    let interface = config.interfaces[index].as_mut()?;
    let slot = interface.endpoints.iter().position(Option::is_none)?;
    if record[2] & 0x0F == 0 {
        return None;
    }
    let size = u16_at(record, 4);
    interface.endpoints[slot] = Some(Endpoint {
        address: record[2],
        max_packet: size & 0x07FF,
        interval: record[6],
        attributes: record[3],
        extra: ((size >> 11) & 0x3).min(2) as u8,
        max_burst: 0,
        bytes_per_interval: 0,
    });
    Some((index, slot))
}

/// The record starting at `at`: its `bLength` must be at least 2 and fit.
fn record_at(chain: &[u8], at: usize) -> Result<&[u8], Error> {
    let len = usize::from(*chain.get(at).ok_or(Error::Short)?);
    if len < 2 {
        return Err(Error::BadLength);
    }
    chain.get(at..at + len).ok_or(Error::BadLength)
}

fn expect_len(record: &[u8], len: usize) -> Result<(), Error> {
    if record.len() < len {
        Err(Error::BadLength)
    } else {
        Ok(())
    }
}

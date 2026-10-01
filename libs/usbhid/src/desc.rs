//! Standard USB descriptors (USB 2.0 ch. 9.6) and the HID class descriptor
//! (HID 1.11 6.2.1), as far as `usbd` needs them: identify the device, then
//! find a HID interface and its interrupt-IN endpoint.
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
}

/// Interface class codes.
pub const CLASS_HID: u8 = 3;
/// `bInterfaceSubClass` of a HID interface that supports the boot protocol.
pub const SUBCLASS_BOOT: u8 = 1;

/// Sizes of the fixed descriptors.
pub const DEVICE_LEN: usize = 18;
pub const CONFIG_LEN: usize = 9;
const INTERFACE_LEN: usize = 9;
const ENDPOINT_LEN: usize = 7;
/// A HID descriptor with one class descriptor entry (the report descriptor).
const HID_LEN: usize = 9;

/// HID interfaces one configuration may report; more are ignored (v1 binds
/// one interface per device, `docs/usb-hid-plan.md` risk 8).
pub const MAX_INTERFACES: usize = 8;

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

/// An endpoint of a HID interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `bEndpointAddress`: number in the low nibble, bit 7 set for IN.
    pub address: u8,
    /// Bytes per packet (`wMaxPacketSize` bits 0..=10).
    pub max_packet: u16,
    /// `bInterval`, in the encoding of the device's speed.
    pub interval: u8,
}

impl Endpoint {
    /// The endpoint number (1..=15).
    pub fn number(&self) -> u8 {
        self.address & 0x0F
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

/// The HID interfaces of one configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// `bConfigurationValue`, the argument of `SET_CONFIGURATION`.
    pub value: u8,
    /// `wTotalLength`.
    pub total_len: u16,
    interfaces: [Option<HidInterface>; MAX_INTERFACES],
}

impl Config {
    /// Every HID interface found, in descriptor order.
    pub fn hid_interfaces(&self) -> impl Iterator<Item = &HidInterface> {
        self.interfaces.iter().flatten()
    }

    /// The first boot keyboard or mouse (alternate setting 0) with an
    /// interrupt-IN endpoint: what v1 binds.
    pub fn first_boot(&self) -> Option<HidInterface> {
        self.hid_interfaces()
            .find(|i| i.alternate == 0 && i.protocol != Protocol::None && i.endpoint.is_some())
            .copied()
    }

    /// The first HID interface (alternate setting 0) with an interrupt-IN
    /// endpoint, boot-capable or not (a tablet has no boot protocol).
    pub fn first_hid(&self) -> Option<HidInterface> {
        self.hid_interfaces()
            .find(|i| i.alternate == 0 && i.endpoint.is_some())
            .copied()
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
    // Index into `interfaces` of the HID interface records attach to; `None`
    // while inside a non-HID interface (or one past the table).
    let mut current: Option<usize> = None;
    let mut at = usize::from(chain[0]);
    while at < chain.len() {
        let record = record_at(chain, at)?;
        match record[1] {
            kind::INTERFACE => {
                expect_len(record, INTERFACE_LEN)?;
                current = None;
                if record[5] == CLASS_HID && found < MAX_INTERFACES {
                    config.interfaces[found] = Some(HidInterface {
                        number: record[2],
                        alternate: record[3],
                        protocol: Protocol::from_byte(record[6], record[7]),
                        report_len: 0,
                        endpoint: None,
                    });
                    current = Some(found);
                    found += 1;
                }
            }
            kind::HID => {
                expect_len(record, HID_LEN)?;
                if let Some(hid) = current.and_then(|i| config.interfaces[i].as_mut()) {
                    if record[6] == kind::REPORT {
                        hid.report_len = u16_at(record, 7);
                    }
                }
            }
            kind::ENDPOINT => {
                expect_len(record, ENDPOINT_LEN)?;
                let interrupt_in = record[2] & 0x80 != 0 && record[3] & 0x03 == 3;
                if let Some(hid) = current.and_then(|i| config.interfaces[i].as_mut()) {
                    if interrupt_in && hid.endpoint.is_none() && record[2] & 0x0F != 0 {
                        hid.endpoint = Some(Endpoint {
                            address: record[2],
                            max_packet: u16_at(record, 4) & 0x07FF,
                            interval: record[6],
                        });
                    }
                }
            }
            _ => {}
        }
        at += record.len();
    }
    Ok(config)
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

//! The HID service record: the attributes of the HID profile that `btd`
//! needs, read out of a `ServiceSearchAttributeResponse`'s attribute lists.
//!
//! The lists are a sequence of records, each a sequence of
//! `attribute id, value` pairs. The first record that names the HID service
//! class and carries a report descriptor wins; a record that does not fit the
//! profile (a descriptor of the wrong type or past the bound) is skipped, not
//! trusted.

use alloc::string::String;
use alloc::vec::Vec;

use crate::elem::{parse, Budget, Elem, Kind};
use crate::{Error, MAX_DESCRIPTOR};

/// Attribute ids (HID profile 1.1.1, section 5.3.4.1 and the SDP base set).
pub mod attr {
    pub const SERVICE_CLASS_ID_LIST: u16 = 0x0001;
    pub const SERVICE_NAME: u16 = 0x0100;
    pub const DEVICE_SUBCLASS: u16 = 0x0202;
    pub const VIRTUAL_CABLE: u16 = 0x0204;
    pub const RECONNECT_INITIATE: u16 = 0x0205;
    pub const DESCRIPTOR_LIST: u16 = 0x0206;
    pub const NORMALLY_CONNECTABLE: u16 = 0x020D;
    pub const BOOT_DEVICE: u16 = 0x020E;
}

/// The Human Interface Device service class.
pub const UUID_HID: u16 = 0x1124;
/// `HIDDescriptorList` entry type: a Report descriptor.
const TYPE_REPORT_DESCRIPTOR: u32 = 0x22;
/// Characters of the service name kept.
pub const MAX_NAME: usize = 64;

/// `HIDDeviceSubclass` bits (the minor device class, keyboard and pointing).
pub mod subclass {
    pub const KEYBOARD: u8 = 0x40;
    pub const POINTING: u8 = 0x80;
}

/// What a device says about itself in its HID record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HidRecord {
    /// The report descriptor (`HIDDescriptorList`), at most
    /// [`MAX_DESCRIPTOR`] octets.
    pub descriptor: Vec<u8>,
    /// `HIDDeviceSubclass` (a [`subclass`] bit set; 0 when absent).
    pub subclass: u8,
    /// The device pages the host to reconnect (`HIDReconnectInitiate`;
    /// defaults to true, as the profile does).
    pub reconnect_initiate: bool,
    /// The device waits to be paged (`HIDNormallyConnectable`).
    pub normally_connectable: bool,
    /// `HIDBootDevice`: the device supports the boot protocol.
    pub boot_device: bool,
    pub virtual_cable: bool,
    /// The service name: printable ASCII, cut to [`MAX_NAME`].
    pub name: String,
}

/// Read the HID record out of the (reassembled) attribute lists.
pub fn parse_attribute_lists(bytes: &[u8]) -> Result<HidRecord, Error> {
    let mut budget = Budget::new();
    let (outer, rest) = parse(bytes)?;
    if !rest.is_empty() || outer.kind != Kind::Seq {
        return Err(Error::BadElement);
    }
    for record in outer.children()? {
        budget.visit()?;
        if let Some(found) = read_record(record?, &mut budget)? {
            return Ok(found);
        }
    }
    Err(Error::NoHidRecord)
}

/// One record; `None` when it is not a usable HID record.
fn read_record(record: Elem<'_>, budget: &mut Budget) -> Result<Option<HidRecord>, Error> {
    let mut found = HidRecord {
        reconnect_initiate: true,
        ..HidRecord::default()
    };
    let (mut is_hid, mut has_descriptor) = (false, false);
    let mut items = record.children()?;
    while let Some(id) = items.next() {
        let id = id?;
        let value = items.next().ok_or(Error::BadElement)??;
        budget.visit()?;
        budget.visit()?;
        let Some(id) = id.uint().filter(|_| id.data.len() == 2) else {
            continue;
        };
        match id as u16 {
            attr::SERVICE_CLASS_ID_LIST => is_hid = lists_hid(value, budget)?,
            attr::SERVICE_NAME => found.name = clean_name(value.data),
            attr::DEVICE_SUBCLASS => found.subclass = value.uint().unwrap_or(0) as u8,
            attr::VIRTUAL_CABLE => found.virtual_cable = value.boolean().unwrap_or(false),
            attr::RECONNECT_INITIATE => found.reconnect_initiate = value.boolean().unwrap_or(true),
            attr::NORMALLY_CONNECTABLE => {
                found.normally_connectable = value.boolean().unwrap_or(false)
            }
            attr::BOOT_DEVICE => found.boot_device = value.boolean().unwrap_or(false),
            attr::DESCRIPTOR_LIST => {
                if let Some(descriptor) = read_descriptor(value, budget)? {
                    found.descriptor = descriptor;
                    has_descriptor = true;
                }
            }
            _ => {}
        }
    }
    Ok((is_hid && has_descriptor).then_some(found))
}

/// Whether a `ServiceClassIDList` names the HID service.
fn lists_hid(list: Elem<'_>, budget: &mut Budget) -> Result<bool, Error> {
    let mut hid = false;
    for class in list.children()? {
        budget.visit()?;
        hid |= class?.uuid16() == Some(UUID_HID);
    }
    Ok(hid)
}

/// The first report descriptor in a `HIDDescriptorList`, a sequence of
/// `[type, descriptor]` sequences. A descriptor past the bound makes the
/// record unusable rather than truncated: a cut descriptor decodes wrongly.
fn read_descriptor(list: Elem<'_>, budget: &mut Budget) -> Result<Option<Vec<u8>>, Error> {
    for entry in list.children()? {
        budget.visit()?;
        let mut parts = entry?.children()?;
        let (Some(ty), Some(data)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (ty, data) = (ty?, data?);
        budget.visit()?;
        budget.visit()?;
        if ty.uint() == Some(TYPE_REPORT_DESCRIPTOR)
            && data.kind == Kind::Text
            && !data.data.is_empty()
            && data.data.len() <= MAX_DESCRIPTOR
        {
            return Ok(Some(data.data.to_vec()));
        }
    }
    Ok(None)
}

/// A stranger's text: printable ASCII only, cut short.
fn clean_name(raw: &[u8]) -> String {
    raw.iter()
        .take_while(|&&b| b != 0)
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .take(MAX_NAME)
        .map(|&b| char::from(b))
        .collect()
}

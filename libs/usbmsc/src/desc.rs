//! Find a Bulk-Only mass-storage interface in a configuration descriptor
//! chain (USB 2.0 9.6, USB 3.2 9.6.7, USB MSC overview 2 and BOT 4).
//!
//! A configuration descriptor is a chain of `(bLength, bDescriptorType, ...)`
//! records `wTotalLength` bytes long. The walk trusts nothing: a zero or
//! overlong `bLength` ends it with [`Error::BadLength`], a known record too
//! short for its fields is refused the same way, and unknown records are
//! skipped by their length. Only alternate setting 0 of an interface counts.

use crate::Error;

/// Descriptor types.
pub mod kind {
    pub const DEVICE: u8 = 1;
    pub const CONFIGURATION: u8 = 2;
    pub const INTERFACE: u8 = 4;
    pub const ENDPOINT: u8 = 5;
    /// SuperSpeed Endpoint Companion (USB 3.2 9.6.7).
    pub const SS_COMPANION: u8 = 0x30;
}

/// Interface class, subclass and protocol of a Bulk-Only SCSI device.
pub const CLASS_MASS_STORAGE: u8 = 0x08;
pub const SUBCLASS_SCSI: u8 = 0x06;
pub const PROTOCOL_BOT: u8 = 0x50;

const CONFIG_LEN: usize = 9;
const INTERFACE_LEN: usize = 9;
const ENDPOINT_LEN: usize = 7;
const COMPANION_LEN: usize = 6;
/// Endpoint transfer type bits (`bmAttributes` 1:0) of a bulk endpoint.
const BULK: u8 = 2;
/// The largest bulk packet: 1024 on SuperSpeed, 512 on high speed.
pub const MAX_BULK_PACKET: u16 = 1024;
/// The largest `bMaxBurst` (16 packets per burst, encoded as 15).
pub const MAX_BURST: u8 = 15;

/// One bulk endpoint of the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkEndpoint {
    /// `bEndpointAddress`: number in the low nibble, bit 7 set for IN.
    pub address: u8,
    /// `wMaxPacketSize` bits 0..=10 (512 high speed, 1024 SuperSpeed).
    pub max_packet: u16,
    /// The companion's `bMaxBurst` (0 when there is none: USB 2).
    pub max_burst: u8,
}

impl BulkEndpoint {
    /// The endpoint number (1..=15).
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }

    /// Whether this is an IN (device-to-host) endpoint.
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

/// A Bulk-Only Transport interface and the configuration that holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotInterface {
    /// `bConfigurationValue` to select with SET_CONFIGURATION.
    pub config_value: u8,
    /// `bInterfaceNumber`: the `wIndex` of the class requests.
    pub number: u8,
    pub bulk_in: BulkEndpoint,
    pub bulk_out: BulkEndpoint,
}

/// `wTotalLength` from a configuration descriptor header.
pub fn config_total_len(header: &[u8]) -> Result<u16, Error> {
    if header.len() < CONFIG_LEN {
        return Err(Error::Short);
    }
    if header[1] != kind::CONFIGURATION {
        return Err(Error::Malformed);
    }
    let total = u16::from_le_bytes([header[2], header[3]]);
    if usize::from(total) < CONFIG_LEN || usize::from(header[0]) < CONFIG_LEN {
        return Err(Error::BadLength);
    }
    Ok(total)
}

/// The interface being walked.
#[derive(Clone, Copy)]
struct Current {
    number: u8,
    bot: bool,
    bulk_in: Option<BulkEndpoint>,
    bulk_out: Option<BulkEndpoint>,
    /// Which endpoint a following companion belongs to (true: IN).
    last_in: Option<bool>,
}

/// The first Bulk-Only SCSI interface (alternate setting 0) with one bulk-IN
/// and one bulk-OUT endpoint, or `Ok(None)` when the configuration has none.
pub fn find_bot(chain: &[u8]) -> Result<Option<BotInterface>, Error> {
    let total = usize::from(config_total_len(chain)?);
    let chain = chain.get(..total).ok_or(Error::BadLength)?;
    let config_value = chain[5];
    let mut at = usize::from(chain[0]);
    let mut current: Option<Current> = None;
    // Every record is at least two bytes long, so this bounds the walk.
    while at < chain.len() {
        let len = usize::from(chain[at]);
        if len < 2 || at + len > chain.len() {
            return Err(Error::BadLength);
        }
        let record = &chain[at..at + len];
        at += len;
        match record[1] {
            kind::INTERFACE => {
                if let Some(found) = complete(config_value, current.take()) {
                    return Ok(Some(found));
                }
                if len < INTERFACE_LEN {
                    return Err(Error::BadLength);
                }
                let bot = record[3] == 0
                    && record[5] == CLASS_MASS_STORAGE
                    && record[6] == SUBCLASS_SCSI
                    && record[7] == PROTOCOL_BOT;
                current = Some(Current {
                    number: record[2],
                    bot,
                    bulk_in: None,
                    bulk_out: None,
                    last_in: None,
                });
            }
            kind::ENDPOINT => {
                if len < ENDPOINT_LEN {
                    return Err(Error::BadLength);
                }
                let Some(iface) = current.as_mut().filter(|iface| iface.bot) else {
                    continue;
                };
                iface.last_in = None;
                let address = record[2];
                let max_packet = u16::from_le_bytes([record[4], record[5]]) & 0x07FF;
                let usable = record[3] & 0x03 == BULK
                    && address & 0x0F != 0
                    && address & 0x70 == 0
                    && (8..=MAX_BULK_PACKET).contains(&max_packet);
                if !usable {
                    continue;
                }
                let endpoint = BulkEndpoint {
                    address,
                    max_packet,
                    max_burst: 0,
                };
                let slot = if endpoint.is_in() {
                    &mut iface.bulk_in
                } else {
                    &mut iface.bulk_out
                };
                // The first endpoint of each direction wins.
                if slot.is_none() {
                    *slot = Some(endpoint);
                    iface.last_in = Some(endpoint.is_in());
                }
            }
            kind::SS_COMPANION => {
                if len < COMPANION_LEN {
                    return Err(Error::BadLength);
                }
                let Some(iface) = current.as_mut().filter(|iface| iface.bot) else {
                    continue;
                };
                let burst = (record[2] & 0x1F).min(MAX_BURST);
                let target = match iface.last_in.take() {
                    Some(true) => iface.bulk_in.as_mut(),
                    Some(false) => iface.bulk_out.as_mut(),
                    None => None,
                };
                if let Some(endpoint) = target {
                    endpoint.max_burst = burst;
                }
            }
            _ => {}
        }
    }
    Ok(complete(config_value, current))
}

/// The interface as found, when it is a usable BOT interface.
fn complete(config_value: u8, current: Option<Current>) -> Option<BotInterface> {
    let current = current.filter(|iface| iface.bot)?;
    Some(BotInterface {
        config_value,
        number: current.number,
        bulk_in: current.bulk_in?,
        bulk_out: current.bulk_out?,
    })
}

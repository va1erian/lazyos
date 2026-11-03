//! The USB side of the fake dongle: its descriptors and endpoint 0.
//!
//! The shape is the common one for a USB Bluetooth controller (Bluetooth Core
//! vol 4 part B): class `E0/01/01`, interface 0 with an interrupt-IN endpoint
//! for events and a bulk pair for ACL data, interface 1 with the isochronous
//! voice channel (alternate 0 empty, alternate 1 with the two endpoints) that
//! `usbd` must leave alone.

pub const VENDOR: u16 = 0x0A12;
pub const PRODUCT: u16 = 0x0001;
/// `bcdDevice` of the device descriptor.
pub const VERSION_BCD: u16 = 0x0100;

/// Endpoint addresses.
pub const EP_EVENTS: u8 = 0x81;
pub const EP_ACL_IN: u8 = 0x82;
pub const EP_ACL_OUT: u8 = 0x02;
pub const EP_ISO_IN: u8 = 0x83;
pub const EP_ISO_OUT: u8 = 0x03;

/// Max packet sizes: events in 16-byte chunks (so a longer event exercises
/// `usbd`'s reassembly), ACL as a high-speed bulk endpoint.
pub const EVENTS_PACKET: u16 = 16;
pub const BULK_PACKET: u16 = 512;
const ISO_PACKET: u16 = 9;

const DEVICE_CLASS: [u8; 3] = [0xE0, 0x01, 0x01];

#[rustfmt::skip]
pub fn device_descriptor() -> Vec<u8> {
    vec![
        18, 1, 0x00, 0x02,
        DEVICE_CLASS[0], DEVICE_CLASS[1], DEVICE_CLASS[2],
        64,
        VENDOR as u8, (VENDOR >> 8) as u8, PRODUCT as u8, (PRODUCT >> 8) as u8,
        0x00, 0x01,
        1, 2, 0,
        1,
    ]
}

fn endpoint(address: u8, attributes: u8, packet: u16, interval: u8) -> [u8; 7] {
    [
        7,
        5,
        address,
        attributes,
        packet as u8,
        (packet >> 8) as u8,
        interval,
    ]
}

fn interface(number: u8, alternate: u8, endpoints: u8) -> [u8; 9] {
    [
        9,
        4,
        number,
        alternate,
        endpoints,
        DEVICE_CLASS[0],
        DEVICE_CLASS[1],
        DEVICE_CLASS[2],
        0,
    ]
}

pub fn config_descriptor() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&interface(0, 0, 3));
    body.extend_from_slice(&endpoint(EP_EVENTS, 3, EVENTS_PACKET, 1));
    body.extend_from_slice(&endpoint(EP_ACL_IN, 2, BULK_PACKET, 0));
    body.extend_from_slice(&endpoint(EP_ACL_OUT, 2, BULK_PACKET, 0));
    body.extend_from_slice(&interface(1, 0, 0));
    body.extend_from_slice(&interface(1, 1, 2));
    body.extend_from_slice(&endpoint(EP_ISO_IN, 1, ISO_PACKET, 1));
    body.extend_from_slice(&endpoint(EP_ISO_OUT, 1, ISO_PACKET, 1));
    let total = (9 + body.len()) as u16;
    // Bus powered, 100 mA.
    let mut config = vec![9, 2, total as u8, (total >> 8) as u8, 2, 1, 0, 0x80, 50];
    config.append(&mut body);
    config
}

/// A string descriptor (UTF-16LE); string 0 is the language list.
pub fn string_descriptor(index: u8) -> Option<Vec<u8>> {
    let text = match index {
        0 => return Some(vec![4, 3, 0x09, 0x04]),
        1 => "LazyOS",
        2 => "Fake Bluetooth Dongle",
        _ => return None,
    };
    let mut out = vec![0, 3];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out[0] = out.len() as u8;
    Some(out)
}

/// A setup packet as the redirection protocol carries it.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

/// What endpoint 0 does with a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Control {
    /// Success, with the data of an IN request.
    Done(Vec<u8>),
    /// An HCI command arrived (class request 0x20/0): the caller feeds it to
    /// the controller.
    Command(Vec<u8>),
    Stall,
}

const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const GET_DESCRIPTOR: u8 = 6;
const HCI_COMMAND: (u8, u8) = (0x20, 0);

pub fn control(setup: Setup, data: &[u8]) -> Control {
    match (setup.request_type, setup.request) {
        (0x80, GET_DESCRIPTOR) => {
            let reply = match (setup.value >> 8, setup.value as u8) {
                (1, _) => Some(device_descriptor()),
                (2, 0) => Some(config_descriptor()),
                (3, index) => string_descriptor(index),
                _ => None,
            };
            match reply {
                Some(mut bytes) => {
                    bytes.truncate(usize::from(setup.length));
                    Control::Done(bytes)
                }
                None => Control::Stall,
            }
        }
        (0x80..=0x82, GET_STATUS) => Control::Done(vec![0, 0]),
        (0x00..=0x02, CLEAR_FEATURE) => Control::Done(Vec::new()),
        (kind, request) if (kind, request) == HCI_COMMAND => {
            // The request's index names the interface; the data is the packet.
            if setup.index == 0 {
                Control::Command(data.to_vec())
            } else {
                Control::Stall
            }
        }
        _ => Control::Stall,
    }
}

/// `(type, interval, interface, max packet)` of every endpoint, by the
/// redirection protocol's endpoint index, for `ep_info`.
pub fn endpoint_table() -> Vec<(u8, u8, u8, u16, u8)> {
    // type codes: 0 control, 1 iso, 2 bulk, 3 interrupt.
    vec![
        (3, 1, 0, EVENTS_PACKET, EP_EVENTS),
        (2, 0, 0, BULK_PACKET, EP_ACL_IN),
        (2, 0, 0, BULK_PACKET, EP_ACL_OUT),
        (1, 1, 1, ISO_PACKET, EP_ISO_IN),
        (1, 1, 1, ISO_PACKET, EP_ISO_OUT),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(value: u16, length: u16) -> Control {
        control(
            Setup {
                request_type: 0x80,
                request: GET_DESCRIPTOR,
                value,
                index: 0,
                length,
            },
            &[],
        )
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// `tools/bt/judge.py` holds the same bytes: the guest's `USBD:DESC:*`
    /// lines are judged against them.
    #[test]
    fn the_descriptors_are_the_ones_the_judge_expects() {
        assert_eq!(
            hex(&device_descriptor()),
            "12010002e0010140120a0100000101020001"
        );
        assert_eq!(
            hex(&config_descriptor()),
            "0902470002010080320904000003e00101000705810310000107058202000200070502020002000904010000e00101000904010102e00101000705830109000107050301090001"
        );
    }

    #[test]
    fn the_config_chain_is_consistent() {
        let config = config_descriptor();
        assert_eq!(
            usize::from(u16::from_le_bytes([config[2], config[3]])),
            config.len()
        );
        // Every record's length walks to the end.
        let mut at = 0;
        while at < config.len() {
            at += usize::from(config[at]);
        }
        assert_eq!(at, config.len());
    }

    #[test]
    fn descriptors_are_cut_to_the_requested_length() {
        assert_eq!(
            get(0x0100, 8),
            Control::Done(device_descriptor()[..8].to_vec())
        );
        let Control::Done(config) = get(0x0200, 9) else {
            panic!("config");
        };
        assert_eq!(config.len(), 9);
    }

    #[test]
    fn the_product_string_is_utf16() {
        let Control::Done(name) = get(0x0302, 255) else {
            panic!("string");
        };
        assert_eq!(name[0] as usize, name.len());
        assert_eq!(&name[2..6], &[b'F', 0, b'a', 0]);
        assert_eq!(get(0x0309, 255), Control::Stall);
    }

    #[test]
    fn an_hci_command_reaches_the_controller_and_other_requests_stall() {
        let setup = Setup {
            request_type: 0x20,
            request: 0,
            value: 0,
            index: 0,
            length: 3,
        };
        assert_eq!(
            control(setup, &[3, 12, 0]),
            Control::Command(vec![3, 12, 0])
        );
        assert_eq!(
            control(Setup { index: 1, ..setup }, &[3, 12, 0]),
            Control::Stall
        );
        assert_eq!(
            control(
                Setup {
                    request: 9,
                    ..setup
                },
                &[]
            ),
            Control::Stall
        );
    }
}

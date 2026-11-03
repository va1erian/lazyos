//! A tiny stand-in controller: Reset, Read_BD_ADDR and Read_Local_Version,
//! and "unknown command" for everything else. Phase B1's `libs/btsim`
//! replaces it (`main.rs` is the one place that names it).

use std::collections::VecDeque;

use crate::controller::{Controller, HciIn};

const OP_RESET: u16 = 0x0C03;
const OP_READ_LOCAL_VERSION: u16 = 0x1001;
const OP_READ_BD_ADDR: u16 = 0x1009;
const OP_READ_LOCAL_NAME: u16 = 0x0C14;

const EVT_COMMAND_COMPLETE: u8 = 0x0E;
const EVT_COMMAND_STATUS: u8 = 0x0F;
/// Unknown HCI Command (Core spec vol 1 part F, 1.1).
const STATUS_UNKNOWN_COMMAND: u8 = 0x01;

/// What Read_Local_Version reports (HCI 5.0, LMP 5.0, the Linux Foundation's
/// company id: a test controller says so).
pub const HCI_VERSION: u8 = 0x09;
pub const LMP_VERSION: u8 = 0x09;
pub const MANUFACTURER: u16 = 0x05F1;
/// What Read_Local_Name reports, zero padded to the 248 bytes the command
/// returns: its event is longer than any endpoint packet, so it crosses
/// `usbd`'s chunk reassembly (16 packets of the events endpoint).
pub const LOCAL_NAME: &str = "LazyOS Fake Controller";

pub struct Stub {
    /// The address, least significant octet first (the wire order).
    address: [u8; 6],
    queue: VecDeque<HciIn>,
    pub commands: u32,
    pub acl: u32,
}

impl Stub {
    pub fn new(address: [u8; 6]) -> Stub {
        Stub {
            address,
            queue: VecDeque::new(),
            commands: 0,
            acl: 0,
        }
    }

    fn complete(&mut self, opcode: u16, returns: &[u8]) {
        let mut event = vec![EVT_COMMAND_COMPLETE, 0, 1];
        event.extend_from_slice(&opcode.to_le_bytes());
        event.extend_from_slice(returns);
        event[1] = (event.len() - 2) as u8;
        self.queue.push_back(HciIn::Event(event));
    }
}

impl Controller for Stub {
    fn command(&mut self, _now_ms: u64, packet: &[u8]) {
        self.commands += 1;
        let [lo, hi, _length, ..] = packet else {
            return;
        };
        let opcode = u16::from_le_bytes([*lo, *hi]);
        match opcode {
            OP_RESET => self.complete(opcode, &[0]),
            OP_READ_LOCAL_VERSION => {
                let mut returns = vec![0, HCI_VERSION, 0x01, 0x00, LMP_VERSION];
                returns.extend_from_slice(&MANUFACTURER.to_le_bytes());
                returns.extend_from_slice(&[0x01, 0x00]);
                self.complete(opcode, &returns);
            }
            OP_READ_LOCAL_NAME => {
                let mut returns = vec![0];
                returns.extend_from_slice(LOCAL_NAME.as_bytes());
                returns.resize(1 + 248, 0);
                self.complete(opcode, &returns);
            }
            OP_READ_BD_ADDR => {
                let mut returns = vec![0];
                returns.extend_from_slice(&self.address);
                self.complete(opcode, &returns);
            }
            _ => {
                let mut event = vec![EVT_COMMAND_STATUS, 4, STATUS_UNKNOWN_COMMAND, 1];
                event.extend_from_slice(&opcode.to_le_bytes());
                self.queue.push_back(HciIn::Event(event));
            }
        }
    }

    fn acl_out(&mut self, _now_ms: u64, _packet: &[u8]) {
        self.acl += 1;
    }

    fn poll_in(&mut self, _now_ms: u64) -> Option<HciIn> {
        self.queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(stub: &mut Stub, opcode: u16) -> Vec<u8> {
        stub.command(0, &[opcode as u8, (opcode >> 8) as u8, 0]);
        match stub.poll_in(0) {
            Some(HciIn::Event(event)) => event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn reset_completes_with_success() {
        let mut stub = Stub::new([1, 2, 3, 4, 5, 6]);
        assert_eq!(answer(&mut stub, OP_RESET), [0x0E, 4, 1, 0x03, 0x0C, 0]);
        assert!(stub.poll_in(0).is_none());
    }

    #[test]
    fn the_address_comes_back_least_significant_first() {
        let mut stub = Stub::new([0x13, 0x71, 0xDA, 0x7D, 0x1A, 0x00]);
        let event = answer(&mut stub, OP_READ_BD_ADDR);
        assert_eq!(event[..7], [0x0E, 10, 1, 0x09, 0x10, 0, 0x13]);
        assert_eq!(event.len(), 12);
    }

    #[test]
    fn the_version_names_the_manufacturer() {
        let mut stub = Stub::new([0; 6]);
        let event = answer(&mut stub, OP_READ_LOCAL_VERSION);
        assert_eq!(event[1] as usize, event.len() - 2);
        assert_eq!(event[5], 0);
        assert_eq!(event[6], HCI_VERSION);
        assert_eq!(&event[10..12], &MANUFACTURER.to_le_bytes());
    }

    #[test]
    fn the_local_name_is_a_long_event() {
        let mut stub = Stub::new([0; 6]);
        let event = answer(&mut stub, OP_READ_LOCAL_NAME);
        assert_eq!(event.len(), 2 + 3 + 1 + 248);
        assert_eq!(event[1] as usize, event.len() - 2);
        assert_eq!(&event[6..6 + LOCAL_NAME.len()], LOCAL_NAME.as_bytes());
        assert!(event[6 + LOCAL_NAME.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn an_unknown_command_gets_a_command_status() {
        let mut stub = Stub::new([0; 6]);
        assert_eq!(answer(&mut stub, 0xFC00), [0x0F, 4, 1, 1, 0x00, 0xFC]);
    }
}

//! BR/EDR discovery in the model: Inquiry, Inquiry_Cancel, Write_Inquiry_Mode
//! and Remote_Name_Request (docs/bluetooth-plan.md B5). A BR/EDR peer in
//! pairing mode (the user holds its button) answers an inquiry; the others
//! do not, as a bonded keyboard on a desk does not.
//!
//! The model is strict: a page (`Create_Connection`) while an inquiry runs is
//! `Command Disallowed`, as controllers answer it, so a host has to cancel
//! first.

use bthci::inquiry::{self, Entry, InquiryEvent, Mode};
use bthci::opcode::status;
use bthci::BdAddr;

use crate::ctrl::{Peer, SimController, Task};
use crate::hid::HidKind;

/// One inquiry length unit.
const UNIT_MS: u64 = 1_280;
/// When the first result of an inquiry arrives, and the gap between them.
const FIRST_RESULT_MS: u64 = 60;
const RESULT_GAP_MS: u64 = 40;
/// How long a name request for an absent device takes to fail.
const NAME_FAIL_MS: u64 = 2_000;
const NAME_MS: u64 = 30;
/// Class of device: Peripheral major class, keyboard or pointing minor class.
const COD_KEYBOARD: u32 = 0x00_2540;
const COD_POINTER: u32 = 0x00_2580;

impl SimController {
    /// Handle the inquiry commands; false for any other opcode.
    pub(crate) fn inquiry_command(&mut self, op: u16, params: &[u8]) -> bool {
        match op {
            inquiry::WRITE_INQUIRY_MODE => self.write_inquiry_mode(op, params),
            inquiry::INQUIRY => self.start_inquiry(op, params),
            inquiry::INQUIRY_CANCEL => self.cancel_inquiry(op),
            inquiry::REMOTE_NAME_REQUEST => self.name_request(op, params),
            _ => return false,
        }
        true
    }

    fn write_inquiry_mode(&mut self, op: u16, params: &[u8]) {
        let mode = match params.first() {
            Some(0) => Mode::Standard,
            Some(1) => Mode::Rssi,
            Some(2) if self.cfg.extended_inquiry => Mode::Extended,
            _ => return self.complete(op, vec![status::UNKNOWN_COMMAND]),
        };
        self.inquiry_mode = mode;
        self.complete(op, vec![0]);
    }

    fn discoverable(&self) -> Vec<usize> {
        (0..self.peers.len())
            .filter(
                |&i| matches!(&self.peers[i], Peer::Br(p) if p.pairing_mode && !p.is_connected()),
            )
            .collect()
    }

    fn entry(&mut self, index: usize, rank: usize) -> Entry {
        let extended = self.inquiry_mode == Mode::Extended;
        let Peer::Br(p) = &self.peers[index] else {
            unreachable!("only BR/EDR peers are discoverable");
        };
        let mut eir = Vec::new();
        if extended {
            eir.push(p.name.len() as u8 + 1);
            eir.push(0x09);
            eir.extend_from_slice(p.name.as_bytes());
        }
        Entry {
            addr: p.addr,
            class_of_device: match p.kind {
                HidKind::Keyboard => COD_KEYBOARD,
                _ => COD_POINTER,
            },
            rssi: (self.inquiry_mode != Mode::Standard).then(|| -50 - 7 * rank as i8),
            eir,
        }
    }

    fn start_inquiry(&mut self, op: u16, params: &[u8]) {
        if self.inquiry_running {
            return self.status(op, status::COMMAND_DISALLOWED);
        }
        let units = u64::from(params.get(3).copied().unwrap_or(1).max(1));
        self.status(op, 0);
        self.inquiry_running = true;
        let mode = self.inquiry_mode;
        let found = self.discoverable();
        for (rank, index) in found.into_iter().enumerate() {
            let entry = self.entry(index, rank);
            let event = InquiryEvent::Results(vec![entry]).to_event(mode);
            let due = self.followup(op, FIRST_RESULT_MS + RESULT_GAP_MS * rank as u64);
            self.after(due, Task::Inquiry(event));
        }
        let end = self.followup(op, units * UNIT_MS);
        let done = InquiryEvent::Complete { status: 0 }.to_event(mode);
        self.after(end, Task::Inquiry(done));
    }

    fn cancel_inquiry(&mut self, op: u16) {
        if !self.inquiry_running {
            return self.complete(op, vec![status::COMMAND_DISALLOWED]);
        }
        self.inquiry_running = false;
        self.drop_tasks(|task| !matches!(task, Task::Inquiry(_)));
        self.complete(op, vec![0]);
    }

    fn name_request(&mut self, op: u16, params: &[u8]) {
        let Some(addr) = params
            .get(..6)
            .map(|a| BdAddr([a[0], a[1], a[2], a[3], a[4], a[5]]))
        else {
            return self.status(op, status::INVALID_PARAMETERS);
        };
        self.status(op, 0);
        let peer = self
            .peer_index(addr)
            .filter(|i| self.discoverable().contains(i));
        let (code, name, delay) = match peer {
            Some(i) => {
                let Peer::Br(p) = &self.peers[i] else {
                    unreachable!("peer_index finds BR/EDR peers");
                };
                (0, p.name.clone().into_bytes(), NAME_MS)
            }
            None => (status::PAGE_TIMEOUT, Vec::new(), NAME_FAIL_MS),
        };
        let event = InquiryEvent::Name {
            status: code,
            addr,
            name,
        }
        .to_event(Mode::Standard);
        let due = self.followup(op, delay);
        self.after(due, Task::Inquiry(event));
    }

    /// Whether a page may start now (not during an inquiry).
    pub(crate) fn may_page(&self) -> bool {
        !self.inquiry_running
    }
}

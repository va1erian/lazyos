//! Command bookkeeping shared by the loaders: one command in flight, one
//! deadline, and the classification of the events that answer it.

use alloc::vec::Vec;

use bthci::command::Command;
use bthci::Event;

use crate::step::Step;

/// How long a command, or an awaited vendor event, may take. The firmware
/// downloads are measured in seconds on real parts (Linux allows 5 s for the
/// bootloader's result and for the reboot), a plain command answers in
/// milliseconds.
pub(crate) const WAIT_MS: u64 = 5_000;

/// A vendor command packet. `bthci` has no vendor opcodes of its own, so they
/// travel as [`Command::Unknown`], which encodes opcode, length and bytes.
pub(crate) fn command(opcode: u16, params: &[u8]) -> Vec<u8> {
    Command::Unknown {
        opcode,
        params: params.to_vec(),
    }
    .encode()
}

/// An answer, after filtering for the command in flight.
pub(crate) enum Reply {
    /// Command Complete with status 0: the return parameters, status first.
    Complete(Vec<u8>),
    /// The controller refused (Command Status or Complete with this status).
    Refused(u8),
    /// A vendor event (code 0xFF): subevent byte first.
    Vendor(Vec<u8>),
    /// Anything else: ignored.
    Other,
}

/// At most one command in flight and one deadline for it (or for the event
/// the loader is waiting for).
#[derive(Default)]
pub(crate) struct Link {
    pub opcode: Option<u16>,
    pub deadline: Option<u64>,
}

impl Link {
    /// Send `opcode` and expect its Command Complete before `now + WAIT_MS`.
    pub fn send(&mut self, now: u64, opcode: u16, params: &[u8]) -> Step {
        self.opcode = Some(opcode);
        self.deadline = Some(now.saturating_add(WAIT_MS));
        Step::Send(command(opcode, params))
    }

    /// Wait for an event (not a command answer) for `WAIT_MS`.
    pub fn wait_event(&mut self, now: u64) {
        self.opcode = None;
        self.deadline = Some(now.saturating_add(WAIT_MS));
    }

    pub fn idle(&mut self) {
        self.opcode = None;
        self.deadline = None;
    }

    pub fn expired(&self, now: u64) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }

    /// Classify one event packet against the command in flight. A packet that
    /// does not parse, or answers some other command, is [`Reply::Other`].
    pub fn reply(&mut self, packet: &[u8]) -> Reply {
        match Event::parse(packet) {
            Ok(Event::CommandComplete {
                opcode,
                return_params,
                ..
            }) if Some(opcode) == self.opcode => {
                self.opcode = None;
                match return_params.first() {
                    Some(0) => Reply::Complete(return_params),
                    Some(&status) => Reply::Refused(status),
                    None => Reply::Refused(bthci::opcode::status::UNKNOWN_COMMAND),
                }
            }
            Ok(Event::CommandStatus { status, opcode, .. })
                if Some(opcode) == self.opcode && status != 0 =>
            {
                self.opcode = None;
                Reply::Refused(status)
            }
            Ok(Event::Unknown { code: 0xFF, params }) => Reply::Vendor(params),
            _ => Reply::Other,
        }
    }
}

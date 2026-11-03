//! Test scaffolding: a scripted controller and a driver that plays a loader
//! against it, the way `btd` will.

mod hostile;
mod intel;
mod intel_failures;
mod real;
mod realtek;
mod realtek_epatch;
mod seeds;
mod vendor;

use std::collections::{BTreeMap, VecDeque};
use std::string::String;
use std::vec::Vec;

use bthci::Event;

use crate::{Input, Loader, Step};

/// A controller model: answers one command with zero or more event packets.
pub(crate) trait Fake {
    fn respond(&mut self, opcode: u16, params: &[u8]) -> Vec<Vec<u8>>;
}

pub(crate) fn cc(opcode: u16, return_params: &[u8]) -> Vec<u8> {
    Event::CommandComplete {
        num_commands: 1,
        opcode,
        return_params: return_params.to_vec(),
    }
    .encode()
}

pub(crate) fn vendor_event(params: &[u8]) -> Vec<u8> {
    let mut out = std::vec![0xFF, params.len() as u8];
    out.extend_from_slice(params);
    out
}

/// What a play-through produced.
pub(crate) struct Run {
    /// Every command the loader sent: opcode and parameters.
    pub sent: Vec<(u16, Vec<u8>)>,
    /// Every file it asked for, in order, with whether it was required.
    pub files: Vec<(String, bool)>,
    /// The step that ended the run: `Done`, `Failed`, or `Wait` when the
    /// loader is waiting for something the controller never sent.
    pub last: Step,
}

impl Run {
    pub fn opcodes(&self) -> Vec<u16> {
        self.sent.iter().map(|(op, _)| *op).collect()
    }

    pub fn count(&self, opcode: u16) -> usize {
        self.sent.iter().filter(|(op, _)| *op == opcode).count()
    }

    pub fn params(&self, opcode: u16) -> Vec<&[u8]> {
        self.sent
            .iter()
            .filter(|(op, _)| *op == opcode)
            .map(|(_, p)| p.as_slice())
            .collect()
    }
}

pub(crate) type Files = BTreeMap<String, Vec<u8>>;

/// Split a command packet into opcode and parameters, checking the length.
pub(crate) fn split(packet: &[u8]) -> (u16, Vec<u8>) {
    assert!(packet.len() >= 3, "command shorter than its header");
    assert_eq!(usize::from(packet[2]), packet.len() - 3, "length octet");
    (
        u16::from_le_bytes([packet[0], packet[1]]),
        packet[3..].to_vec(),
    )
}

/// Play `loader` against `fake` from `first` until it finishes or stalls.
pub(crate) fn drive<F: Fake>(
    loader: &mut Loader,
    fake: &mut F,
    files: &Files,
    now: u64,
    first: Input<'_>,
) -> Run {
    let mut run = Run {
        sent: Vec::new(),
        files: Vec::new(),
        last: Step::Wait,
    };
    let mut events: VecDeque<Vec<u8>> = VecDeque::new();
    let mut step = loader.step(now, first);
    for _ in 0..200_000 {
        match step {
            Step::Send(packet) => {
                let (opcode, params) = split(&packet);
                events.extend(fake.respond(opcode, &params));
                run.sent.push((opcode, params));
            }
            Step::NeedFile(request) => {
                let found = files.get(&request.name).cloned();
                run.files.push((request.name, request.required));
                step = match found {
                    Some(data) => loader.step(now, Input::File(data)),
                    None => loader.step(now, Input::NoFile),
                };
                continue;
            }
            Step::Wait => {}
            done @ (Step::Done(_) | Step::Failed(_)) => {
                run.last = done;
                return run;
            }
        }
        let Some(event) = events.pop_front() else {
            return run;
        };
        step = loader.step(now, Input::Event(&event));
    }
    panic!("the loader never finished");
}

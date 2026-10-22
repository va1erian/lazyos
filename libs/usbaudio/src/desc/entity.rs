//! AudioControl entities: the class-specific records of the control
//! interface (UAC 1.0 4.3.2, UAC 2.0 4.7.2). Only what playback needs is
//! decoded (terminal types and clocks, the feature unit's master mute, the
//! clock tree); any other unit is kept by id so a chain through it is still
//! recognised as broken rather than misread.

use alloc::vec::Vec;

use super::{u16_at, u32_at, Function, Version, MAX_ENTITIES};

/// AudioControl descriptor subtypes (the same numbers in both revisions up
/// to 7; 0x0A..=0x0C exist only in UAC 2.0).
mod subtype {
    pub const HEADER: u8 = 1;
    pub const INPUT_TERMINAL: u8 = 2;
    pub const OUTPUT_TERMINAL: u8 = 3;
    pub const FEATURE_UNIT: u8 = 6;
    pub const CLOCK_SOURCE: u8 = 0x0A;
    pub const CLOCK_SELECTOR: u8 = 0x0B;
    pub const CLOCK_MULTIPLIER: u8 = 0x0C;
}

/// One entity of the control interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entity {
    /// `kind` is `wTerminalType`; `clock` the UAC 2.0 `bCSourceID` (0 in 1.0).
    InputTerminal {
        id: u8,
        kind: u16,
        clock: u8,
        channels: u8,
    },
    OutputTerminal {
        id: u8,
        kind: u16,
        source: u8,
    },
    /// `master_mute`: the master channel's mute is host-programmable.
    Feature {
        id: u8,
        source: u8,
        master_mute: bool,
    },
    /// `programmable`: the frequency control can be set (UAC 2.0 bmControls
    /// bits 0..=1 are `0b11`).
    ClockSource {
        id: u8,
        programmable: bool,
    },
    ClockSelector {
        id: u8,
        inputs: Vec<u8>,
    },
    ClockMultiplier {
        id: u8,
        source: u8,
    },
    /// A mixer, selector, processing or extension unit: known by id only.
    Other {
        id: u8,
    },
}

impl Entity {
    pub fn id(&self) -> u8 {
        match self {
            Entity::InputTerminal { id, .. }
            | Entity::OutputTerminal { id, .. }
            | Entity::Feature { id, .. }
            | Entity::ClockSource { id, .. }
            | Entity::ClockSelector { id, .. }
            | Entity::ClockMultiplier { id, .. }
            | Entity::Other { id } => *id,
        }
    }
}

/// One class-specific record of the control interface of `function`.
pub(super) fn record(function: &mut Function, record: &[u8]) {
    if record.len() < 4 {
        return;
    }
    if record[2] == subtype::HEADER {
        // `bcdADC` decides the layout of everything after it; the
        // interface protocol said the same on a well-formed device.
        if record.len() >= 5 && u16_at(record, 3) >= 0x0200 {
            function.version = Version::Uac2;
        }
        return;
    }
    let decoded = match function.version {
        Version::Uac1 => uac1(record),
        Version::Uac2 => uac2(record),
    };
    let Some(entity) = decoded else {
        return;
    };
    // Id 0 is "none" in every source field; a repeated id keeps the first.
    if entity.id() != 0
        && function.entities.len() < MAX_ENTITIES
        && function.entity(entity.id()).is_none()
    {
        function.entities.push(entity);
    }
}

fn uac1(record: &[u8]) -> Option<Entity> {
    let id = record[3];
    Some(match record[2] {
        subtype::INPUT_TERMINAL if record.len() >= 12 => Entity::InputTerminal {
            id,
            kind: u16_at(record, 4),
            clock: 0,
            channels: record[7],
        },
        subtype::OUTPUT_TERMINAL if record.len() >= 9 => Entity::OutputTerminal {
            id,
            kind: u16_at(record, 4),
            source: record[7],
        },
        subtype::FEATURE_UNIT if record.len() >= 7 => {
            // bControlSize at 5, then the master channel's controls; bit 0
            // of the first byte is Mute (always settable in 1.0).
            let size = usize::from(record[5]);
            let master_mute = size >= 1 && record.len() > 6 && record[6] & 1 != 0;
            Entity::Feature {
                id,
                source: record[4],
                master_mute,
            }
        }
        subtype::INPUT_TERMINAL | subtype::OUTPUT_TERMINAL | subtype::FEATURE_UNIT => {
            return None
        }
        _ => Entity::Other { id },
    })
}

fn uac2(record: &[u8]) -> Option<Entity> {
    let id = record[3];
    Some(match record[2] {
        subtype::INPUT_TERMINAL if record.len() >= 17 => Entity::InputTerminal {
            id,
            kind: u16_at(record, 4),
            clock: record[7],
            channels: record[8],
        },
        subtype::OUTPUT_TERMINAL if record.len() >= 12 => Entity::OutputTerminal {
            id,
            kind: u16_at(record, 4),
            source: record[7],
        },
        subtype::FEATURE_UNIT if record.len() >= 9 => Entity::Feature {
            id,
            source: record[4],
            // bmaControls(0), 4 bytes: Mute is bits 0..=1, 0b11 settable.
            master_mute: u32_at(record, 5) & 0b11 == 0b11,
        },
        subtype::CLOCK_SOURCE if record.len() >= 8 => Entity::ClockSource {
            id,
            programmable: record[5] & 0b11 == 0b11,
        },
        subtype::CLOCK_SELECTOR if record.len() >= 5 => {
            let count = usize::from(record[4]);
            let inputs = record.get(5..5 + count)?.to_vec();
            Entity::ClockSelector { id, inputs }
        }
        subtype::CLOCK_MULTIPLIER if record.len() >= 5 => Entity::ClockMultiplier {
            id,
            source: record[4],
        },
        subtype::INPUT_TERMINAL
        | subtype::OUTPUT_TERMINAL
        | subtype::FEATURE_UNIT
        | subtype::CLOCK_SOURCE
        | subtype::CLOCK_SELECTOR
        | subtype::CLOCK_MULTIPLIER => return None,
        _ => Entity::Other { id },
    })
}

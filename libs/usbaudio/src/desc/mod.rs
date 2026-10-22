//! The audio functions of a configuration descriptor chain (USB Audio 1.0
//! sections 4.3-4.6, USB Audio 2.0 sections 4.7-4.10).
//!
//! An audio function is an AudioControl interface (class 1, subclass 1) and
//! the AudioStreaming interfaces (subclass 2) that follow it, up to the next
//! AudioControl interface. The control interface's class-specific records
//! are its *entities* (terminals, units, clocks: [`Entity`]); each streaming
//! alternate with an isochronous OUT data endpoint and a PCM type I format is
//! a playback [`Alt`]. Everything else (capture alternates, compressed
//! formats, other classes) is skipped, not refused: a headset's microphone
//! does not stop its speaker from working.
//!
//! The parser works on a copy of the chain, checks every record's length
//! before reading a field, and bounds every list ([`MAX_FUNCTIONS`],
//! [`MAX_ENTITIES`], [`MAX_ALTS`], [`MAX_RANGES`]), so a hostile device costs
//! a bounded amount of work and memory whatever it sends.

mod entity;
mod stream;

use alloc::vec::Vec;

use crate::Error;
pub use entity::Entity;
pub use stream::{Alt, Format, IsoEndpoint, Range, RateSet, Sync};

/// Audio functions kept per configuration.
pub const MAX_FUNCTIONS: usize = 4;
/// Entities kept per function.
pub const MAX_ENTITIES: usize = 64;
/// Playback alternates kept per function.
pub const MAX_ALTS: usize = 32;
/// Rate ranges kept per alternate (a discrete list is one range per rate).
pub const MAX_RANGES: usize = 32;

/// Standard and class descriptor types and subtypes.
pub mod kind {
    pub const INTERFACE: u8 = 4;
    pub const ENDPOINT: u8 = 5;
    pub const CS_INTERFACE: u8 = 0x24;
    pub const CS_ENDPOINT: u8 = 0x25;
    pub const CLASS_AUDIO: u8 = 1;
    pub const SUBCLASS_CONTROL: u8 = 1;
    pub const SUBCLASS_STREAMING: u8 = 2;
    /// `bInterfaceProtocol` of a UAC 2.0 interface (IP_VERSION_02_00).
    pub const PROTOCOL_UAC2: u8 = 0x20;
}

/// Which revision of the class the function follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    Uac1,
    Uac2,
}

/// One audio function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub version: Version,
    /// The AudioControl interface number (class requests to entities go to it).
    pub control_interface: u8,
    pub entities: Vec<Entity>,
    /// Every playback alternate, in descriptor order.
    pub alts: Vec<Alt>,
}

impl Function {
    /// The entity with id `id`.
    pub fn entity(&self, id: u8) -> Option<&Entity> {
        self.entities.iter().find(|entity| entity.id() == id)
    }

    /// The clock source an alternate's terminal runs from (UAC 2.0): its
    /// terminal's clock, through selectors (first input) and multipliers,
    /// at most [`MAX_ENTITIES`] hops. `None` for UAC 1.0 or a broken chain.
    pub fn clock_of(&self, alt: &Alt) -> Option<u8> {
        let mut id = match self.entity(alt.terminal)? {
            Entity::InputTerminal { clock, .. } => *clock,
            _ => return None,
        };
        for _ in 0..MAX_ENTITIES {
            match self.entity(id)? {
                Entity::ClockSource { .. } => return Some(id),
                Entity::ClockSelector { inputs, .. } => id = *inputs.first()?,
                Entity::ClockMultiplier { source, .. } => id = *source,
                _ => return None,
            }
        }
        None
    }

    /// The feature unit fed directly by an alternate's terminal, if it has
    /// a programmable master mute (switched off when the stream starts).
    pub fn mute_unit(&self, alt: &Alt) -> Option<u8> {
        self.entities.iter().find_map(|entity| match entity {
            Entity::Feature {
                id,
                source,
                master_mute: true,
            } if *source == alt.terminal => Some(*id),
            _ => None,
        })
    }

    /// Give every alternate whose terminal runs from `clock` the rates a
    /// UAC 2.0 `GET_RANGE` of that clock returned.
    pub fn set_clock_rates(&mut self, clock: u8, rates: &RateSet) {
        let alts: Vec<usize> = (0..self.alts.len())
            .filter(|&index| self.clock_of(&self.alts[index]) == Some(clock))
            .collect();
        for index in alts {
            self.alts[index].rates = rates.clone();
        }
    }

    /// The clock sources this function's playback alternates run from.
    pub fn clocks(&self) -> Vec<u8> {
        let mut clocks: Vec<u8> = Vec::new();
        for alt in &self.alts {
            if let Some(clock) = self.clock_of(alt) {
                if !clocks.contains(&clock) {
                    clocks.push(clock);
                }
            }
        }
        clocks
    }
}

/// What the walk is attaching records to.
enum Place {
    /// Outside any audio interface.
    None,
    /// The AudioControl interface of the last function.
    Control,
    /// An AudioStreaming alternate of the last function.
    Streaming(stream::Builder),
}

/// Every audio function in the configuration chain `chain` (the
/// `wTotalLength` bytes a GET_DESCRIPTOR returned).
pub fn parse_functions(chain: &[u8]) -> Result<Vec<Function>, Error> {
    let mut functions: Vec<Function> = Vec::new();
    let mut place = Place::None;
    // Streaming interfaces belong to the last AudioControl interface seen;
    // past [`MAX_FUNCTIONS`] that one is not kept, so neither are they.
    let mut owner_kept = false;
    let mut at = 0;
    while at < chain.len() {
        let len = usize::from(chain[at]);
        if len < 2 {
            return Err(Error::BadLength);
        }
        let record = chain.get(at..at + len).ok_or(Error::BadLength)?;
        at += len;
        match record[1] {
            kind::INTERFACE => {
                finish(&mut functions, &mut place);
                // A short interface record ends what came before and starts
                // nothing.
                place = if record.len() >= 9 {
                    open_interface(&mut functions, record, &mut owner_kept)
                } else {
                    Place::None
                };
            }
            kind::CS_INTERFACE => match (&mut place, functions.last_mut()) {
                (Place::Control, Some(function)) => entity::record(function, record),
                (Place::Streaming(builder), Some(function)) => {
                    builder.class_record(function.version, record)
                }
                _ => {}
            },
            kind::ENDPOINT => {
                if let Place::Streaming(builder) = &mut place {
                    builder.endpoint(record);
                }
            }
            kind::CS_ENDPOINT => {
                if let Place::Streaming(builder) = &mut place {
                    builder.class_endpoint(record);
                }
            }
            _ => {}
        }
    }
    finish(&mut functions, &mut place);
    Ok(functions)
}

/// An interface descriptor: a new function, a streaming alternate of the
/// current one, or something else.
fn open_interface(functions: &mut Vec<Function>, record: &[u8], owner_kept: &mut bool) -> Place {
    let (number, alternate, class, subclass, protocol) =
        (record[2], record[3], record[5], record[6], record[7]);
    if class != kind::CLASS_AUDIO {
        return Place::None;
    }
    match subclass {
        kind::SUBCLASS_CONTROL if functions.len() >= MAX_FUNCTIONS => {
            *owner_kept = false;
            Place::None
        }
        kind::SUBCLASS_CONTROL => {
            *owner_kept = true;
            functions.push(Function {
                version: if protocol == kind::PROTOCOL_UAC2 {
                    Version::Uac2
                } else {
                    Version::Uac1
                },
                control_interface: number,
                entities: Vec::new(),
                alts: Vec::new(),
            });
            Place::Control
        }
        kind::SUBCLASS_STREAMING if *owner_kept => {
            Place::Streaming(stream::Builder::new(number, alternate))
        }
        _ => Place::None,
    }
}

/// Close the alternate being built, keeping it when it is playback.
fn finish(functions: &mut [Function], place: &mut Place) {
    if let (Place::Streaming(builder), Some(function)) =
        (core::mem::replace(place, Place::None), functions.last_mut())
    {
        if function.alts.len() < MAX_ALTS {
            if let Some(alt) = builder.build() {
                function.alts.push(alt);
            }
        }
    }
}

/// Little-endian fields of a record whose length the caller checked.
pub(crate) fn u16_at(record: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([record[at], record[at + 1]])
}

pub(crate) fn u24_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([record[at], record[at + 1], record[at + 2], 0])
}

pub(crate) fn u32_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
}

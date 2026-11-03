//! The report layout of a keyboard in report protocol (`docs/bluetooth-plan.md`
//! decision 7): a Bluetooth keyboard's input report is the USB boot layout
//! (modifier byte, reserved byte, six key slots) behind an optional report id,
//! and nothing else is trusted.
//!
//! USB keyboards are bound in boot protocol and never need this; a Bluetooth
//! device is told to stay in report protocol (its descriptor may add media
//! keys the boot report lacks), so its descriptor must say the first report is
//! the familiar one. A keyboard whose descriptor places the fields elsewhere
//! is [`Error::WrongType`] and the caller falls back to boot protocol.

use super::{fields, walk};
use crate::boot::{BootKeyboard, KeyEdge, KEYBOARD_REPORT};
use crate::Error;

/// The Keyboard/Keypad usage page.
const KEYBOARD_PAGE: u16 = 0x07;
/// Bit offset of the six key slots in a boot-layout report.
const KEYS_AT: u32 = 16;
/// The key slots the layout must offer.
const KEY_SLOTS: u32 = 6;

/// Where a keyboard's boot-layout report sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    /// The report id of the key report (`None`: reports carry no id).
    pub report_id: Option<u8>,
}

/// Find the keyboard report in a report descriptor: the first report whose
/// Input holds the eight modifier bits at its start and six key slots at
/// byte 2.
pub fn parse_keyboard(descriptor: &[u8]) -> Result<Keyboard, Error> {
    let mut id = None;
    let mut ids_before = false;
    walk(descriptor, |item| {
        let modifiers = fields(item).any(|((page, use_), field)| {
            (page, use_) == (KEYBOARD_PAGE, 0xE0) && field.bit == 0 && field.bits == 1
        });
        if id.is_none() && modifiers {
            id = Some(item.globals.report_id);
            ids_before = item.ids_used;
        }
    })?;
    let id = id.ok_or(Error::WrongType)?;
    let mut keys = false;
    walk(descriptor, |item| {
        let array = item.flags & 0b11 == 0;
        if item.globals.report_id == id
            && array
            && item.globals.page == KEYBOARD_PAGE
            && item.start == KEYS_AT
            && item.globals.size == 8
            && item.globals.count >= KEY_SLOTS
        {
            keys = true;
        }
    })?;
    if !keys {
        return Err(Error::WrongType);
    }
    Ok(Keyboard {
        report_id: ids_before.then_some(id),
    })
}

/// Turns successive keyboard reports into key edges, like a boot keyboard.
#[derive(Clone, Debug)]
pub struct Decoder {
    layout: Keyboard,
    keys: BootKeyboard,
}

impl Decoder {
    pub fn new(layout: Keyboard) -> Decoder {
        Decoder {
            layout,
            keys: BootKeyboard::new(),
        }
    }

    /// Decode `report` (as sent, id byte included when the layout has one);
    /// `false` when it is another report or too short, which changes nothing.
    pub fn feed(&mut self, report: &[u8], emit: impl FnMut(KeyEdge)) -> bool {
        let data = match self.layout.report_id {
            Some(id) => match report.split_first() {
                Some((&first, rest)) if first == id => rest,
                _ => return false,
            },
            None => report,
        };
        let data = &data[..data.len().min(KEYBOARD_REPORT)];
        self.keys.feed(data, emit).is_ok()
    }

    /// Release every held key (the device went away).
    pub fn release_all(&mut self, emit: impl FnMut(KeyEdge)) {
        self.keys.release_all(emit);
    }

    pub fn is_held(&self, usage: u16) -> bool {
        self.keys.is_held(usage)
    }
}

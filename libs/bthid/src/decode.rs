//! Choosing and running a report decoder (`docs/bluetooth-plan.md` decision
//! 7): report protocol by default, boot protocol as the fallback.
//!
//! A [`Plan`] is made once, when a device is bonded, from its report
//! descriptor and the `HIDDeviceSubclass` of its SDP record, and stored with
//! the bond (the descriptor is; the plan is cheap to recompute). The source
//! class a device registers follows the plan, which follows the bond's record,
//! never a live report.

use usbhid::boot::{parse_mouse, BootKeyboard, BootMouse, KeyEdge};
use usbhid::report::keyboard::{self, parse_keyboard};
use usbhid::report::{self, parse_pointer, Out};

/// `HIDDeviceSubclass` bits.
const SUBCLASS_KEYBOARD: u8 = 0x40;
const SUBCLASS_POINTING: u8 = 0x80;

/// Which protocol the device runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reports are described by the descriptor.
    Report,
    /// The device is switched with `SET_PROTOCOL(boot)` and sends the fixed
    /// boot reports.
    Boot,
}

/// What a device is and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub keyboard: bool,
    pub pointer: bool,
    /// The pointer reports positions (a tablet).
    pub absolute: bool,
}

impl Plan {
    /// Plan for `descriptor`; `None` when it is neither a keyboard nor a
    /// pointer and the subclass names neither (nothing to register).
    pub fn new(descriptor: &[u8], subclass: u8) -> Option<Plan> {
        let kbd = parse_keyboard(descriptor).is_ok();
        let ptr = parse_pointer(descriptor).ok();
        if kbd || ptr.is_some() {
            return Some(Plan {
                mode: Mode::Report,
                keyboard: kbd,
                pointer: ptr.is_some(),
                absolute: ptr.is_some_and(|p| p.absolute()),
            });
        }
        // The descriptor says nothing usable; the boot protocol may still do.
        // A device in boot protocol is one or the other.
        let (keyboard, pointer) = if subclass & SUBCLASS_KEYBOARD != 0 {
            (true, false)
        } else {
            (false, subclass & SUBCLASS_POINTING != 0)
        };
        (keyboard || pointer).then_some(Plan {
            mode: Mode::Boot,
            keyboard,
            pointer,
            absolute: false,
        })
    }

    /// The same plan without the keyboard half (a combo device whose key
    /// is not authenticated keeps its pointer). `None` when nothing is left.
    pub fn without_keyboard(self) -> Option<Plan> {
        (self.pointer).then_some(Plan {
            keyboard: false,
            ..self
        })
    }

    pub fn needs_boot_protocol(&self) -> bool {
        self.mode == Mode::Boot
    }
}

/// What a report produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key(KeyEdge),
    Pointer(Out),
}

enum Keys {
    None,
    Report(keyboard::Decoder),
    Boot(BootKeyboard),
}

enum Pointer {
    None,
    Report(report::Decoder),
    Boot(BootMouse),
}

/// Turns the reports of one device into edges and deltas.
pub struct Decoder {
    keys: Keys,
    pointer: Pointer,
}

impl Decoder {
    /// A decoder for `plan`, reading the layouts out of `descriptor` (the
    /// one the plan was made from; a layout that no longer parses simply
    /// leaves that half silent).
    pub fn new(plan: Plan, descriptor: &[u8]) -> Decoder {
        match plan.mode {
            Mode::Report => Decoder {
                keys: match parse_keyboard(descriptor) {
                    Ok(layout) if plan.keyboard => Keys::Report(keyboard::Decoder::new(layout)),
                    _ => Keys::None,
                },
                pointer: match parse_pointer(descriptor) {
                    Ok(layout) if plan.pointer => Pointer::Report(report::Decoder::new(layout)),
                    _ => Pointer::None,
                },
            },
            Mode::Boot => Decoder {
                keys: if plan.keyboard {
                    Keys::Boot(BootKeyboard::new())
                } else {
                    Keys::None
                },
                pointer: if plan.pointer {
                    Pointer::Boot(BootMouse::new())
                } else {
                    Pointer::None
                },
            },
        }
    }

    /// Decode one input report (id byte included in report protocol, absent
    /// in boot protocol). `false` when no half owns it.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(Event)) -> bool {
        let mut owned = false;
        match &mut self.keys {
            Keys::None => {}
            Keys::Report(d) => owned |= d.feed(report, |e| emit(Event::Key(e))),
            // Boot protocol has one report format per device: the keyboard
            // owns every report of a keyboard.
            Keys::Boot(d) => owned |= d.feed(report, |e| emit(Event::Key(e))).is_ok(),
        }
        match &mut self.pointer {
            Pointer::None => {}
            Pointer::Report(d) => owned |= d.feed(report, |o| emit(Event::Pointer(o))),
            Pointer::Boot(d) => {
                if let Ok(parsed) = parse_mouse(report) {
                    d.feed(&parsed, |o| emit(Event::Pointer(Out::Mouse(o))));
                    owned = true;
                }
            }
        }
        owned
    }

    /// Let go of everything held: the link is gone, no key may stay down.
    pub fn release_all(&mut self, mut emit: impl FnMut(Event)) {
        match &mut self.keys {
            Keys::None => {}
            Keys::Report(d) => d.release_all(|e| emit(Event::Key(e))),
            Keys::Boot(d) => d.release_all(|e| emit(Event::Key(e))),
        }
        match &mut self.pointer {
            Pointer::None => {}
            Pointer::Report(d) => d.release_all(|o| emit(Event::Pointer(o))),
            Pointer::Boot(d) => d.release_all(|o| emit(Event::Pointer(Out::Mouse(o)))),
        }
    }
}

//! A configured boot keyboard or mouse, publishing onto the raw input bus
//! through its own kernel source (`docs/usb-hid-plan.md` U1).
//!
//! Each HID interface is one source of its class, so the kernel stamps a
//! device id of its own and refuses records the class may not carry. The
//! driver holds no input policy: reports become edges and deltas, nothing
//! more (no cursor, no clamping, no repeat).

use alloc::format;
use alloc::vec::Vec;

use usbhid::boot::{parse_mouse, BootKeyboard, BootMouse, KeyEdge, MouseOut};
use usbhid::desc::Protocol;
use user::sys::{self, raw_kind, source_class, SourceRecord, SOURCE_MAX_BATCH};

use super::Error;

/// The decoder for one interface's boot reports.
enum Decoder {
    Keyboard(BootKeyboard),
    Mouse(BootMouse),
}

pub(super) struct Hid {
    source: u64,
    decoder: Decoder,
    /// Records the kernel refused or throttled (it counts them too).
    pub(super) refused: u64,
}

impl Hid {
    /// Register a source for a boot `protocol` interface.
    pub(super) fn new(protocol: Protocol) -> Result<Hid, Error> {
        let (class, decoder) = match protocol {
            Protocol::Keyboard => (
                source_class::KEYBOARD,
                Decoder::Keyboard(BootKeyboard::new()),
            ),
            Protocol::Mouse => (source_class::POINTER, Decoder::Mouse(BootMouse::new())),
            Protocol::None => return Err(Error::Descriptor("not a boot interface")),
        };
        let source = sys::input_source_register(class).map_err(Error::Source)?;
        Ok(Hid {
            source,
            decoder,
            refused: 0,
        })
    }

    /// Decode one report and publish what it means. `trace` echoes each edge
    /// on serial for the harness.
    pub(super) fn report(&mut self, report: &[u8], trace: bool) {
        if trace {
            let hex: alloc::string::String = report.iter().map(|b| format!("{b:02x}")).collect();
            sys::write_str(&format!("USBD:REPORT {hex}\n"));
        }
        let mut records = Vec::new();
        match &mut self.decoder {
            Decoder::Keyboard(keyboard) => {
                let _ = keyboard.feed(report, |edge: KeyEdge| {
                    if trace {
                        trace_key(&edge);
                    }
                    records.push(key(edge));
                });
            }
            Decoder::Mouse(mouse) => {
                if let Ok(parsed) = parse_mouse(report) {
                    mouse.feed(&parsed, |out| records.push(pointer(out)));
                }
            }
        }
        self.publish(&records);
    }

    fn publish(&mut self, records: &[SourceRecord]) {
        for batch in records.chunks(SOURCE_MAX_BATCH) {
            match sys::input_source_publish(self.source, batch) {
                Ok(accepted) => self.refused += (batch.len() - accepted) as u64,
                Err(_) => self.refused += batch.len() as u64,
            }
        }
    }

    /// Detach: release what the device held. The kernel would do the same
    /// on close; doing it through the decoder keeps both views in step.
    pub(super) fn close(mut self, trace: bool) {
        let mut records = Vec::new();
        match &mut self.decoder {
            Decoder::Keyboard(keyboard) => keyboard.release_all(|edge| {
                if trace {
                    trace_key(&edge);
                }
                records.push(key(edge));
            }),
            Decoder::Mouse(mouse) => mouse.release_all(|out| records.push(pointer(out))),
        }
        self.publish(&records);
        let _ = sys::input_source_close(self.source);
    }
}

/// The harness's evidence for one key edge `usbd` published.
fn trace_key(edge: &KeyEdge) {
    sys::write_str(&format!(
        "USBD:KEY usage={:#x} {}\n",
        edge.usage,
        if edge.pressed { "down" } else { "up" }
    ));
}

fn key(edge: KeyEdge) -> SourceRecord {
    SourceRecord {
        kind: raw_kind::KEY,
        code: edge.usage,
        value: i32::from(edge.pressed),
    }
}

/// Bus pointer records (`kernel/src/input/bus.rs`, `pointer`): HID axes
/// already match the bus (`dy > 0` down, wheel > 0 up).
fn pointer(out: MouseOut) -> SourceRecord {
    match out {
        MouseOut::Motion { dx, dy } => SourceRecord {
            kind: raw_kind::REL_MOTION,
            code: 0,
            value: (u32::from(dx as u16) | u32::from(dy as u16) << 16) as i32,
        },
        MouseOut::Wheel(notches) => SourceRecord {
            kind: raw_kind::SCROLL,
            code: 0,
            value: notches,
        },
        MouseOut::Button { usage, pressed } => SourceRecord {
            kind: raw_kind::BUTTON,
            code: usage,
            value: i32::from(pressed),
        },
    }
}

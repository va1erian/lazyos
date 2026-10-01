//! A configured keyboard, mouse or tablet, publishing onto the raw input bus
//! through its own kernel source (`docs/usb-hid-plan.md` U1, U4). Boot
//! keyboards and mice are decoded by their fixed boot reports; a
//! report-protocol pointer (a tablet) by the layout its report descriptor
//! gave, as absolute positions (`ABS_MOTION`, `0..=0xFFFF`) or, for a
//! relative one, motion.
//!
//! Each HID interface is one source of its class, so the kernel stamps a
//! device id of its own and refuses records the class may not carry. The
//! driver holds no input policy: reports become edges and deltas, nothing
//! more (no cursor, no clamping, no repeat).

use alloc::format;
use alloc::vec::Vec;

use usbhid::boot::{parse_mouse, BootKeyboard, BootMouse, KeyEdge, MouseOut};
use usbhid::desc::Protocol;
use usbhid::report::{self, Out, Pointer};
use user::sys::{self, raw_kind, source_class, SourceRecord, SOURCE_MAX_BATCH};

use super::Error;

/// The decoder for one interface's reports.
enum Decoder {
    Keyboard(BootKeyboard),
    Mouse(BootMouse),
    Report(report::Decoder),
}

pub(super) struct Hid {
    source: u64,
    decoder: Decoder,
    /// Records the kernel refused or throttled (it counts them too).
    pub(super) refused: u64,
}

impl Hid {
    /// Register a source for a boot `protocol` interface, or for the
    /// report-protocol pointer `layout` describes.
    pub(super) fn new(protocol: Protocol, layout: Option<Pointer>) -> Result<Hid, Error> {
        let (class, decoder) = match (protocol, layout) {
            (Protocol::Keyboard, _) => (
                source_class::KEYBOARD,
                Decoder::Keyboard(BootKeyboard::new()),
            ),
            (Protocol::Mouse, _) => (source_class::POINTER, Decoder::Mouse(BootMouse::new())),
            (Protocol::None, Some(layout)) => {
                let decoder = report::Decoder::new(layout);
                let class = if decoder.absolute() {
                    source_class::TABLET
                } else {
                    source_class::POINTER
                };
                (class, Decoder::Report(decoder))
            }
            (Protocol::None, None) => return Err(Error::Descriptor("no boot or report layout")),
        };
        let source = sys::input_source_register(class).map_err(Error::Source)?;
        Ok(Hid {
            source,
            decoder,
            refused: 0,
        })
    }

    /// Decode one report and publish what it means; returns whether it
    /// pressed a key. `trace` echoes each edge on serial for the harness.
    pub(super) fn report(&mut self, report: &[u8], trace: bool) -> bool {
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
            Decoder::Report(decoder) => {
                decoder.feed(report, |out| records.push(report_out(out)));
            }
        }
        self.publish(&records);
        records
            .iter()
            .any(|r| r.kind == raw_kind::KEY && r.value == 1)
    }

    /// Whether this is an absolute pointer (a tablet).
    pub(super) fn tablet(&self) -> bool {
        matches!(&self.decoder, Decoder::Report(decoder) if decoder.absolute())
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
            Decoder::Report(decoder) => decoder.release_all(|out| records.push(report_out(out))),
        }
        self.publish(&records);
        let _ = sys::input_source_close(self.source);
    }
}

/// The harness's evidence for one key edge `usbd` published.
/// `t=` is the tick (10 ms) it was published, so a harness can tell `usbd`'s
/// delivery latency from `inputd`'s (which stamps its own lines the same way).
fn trace_key(edge: &KeyEdge) {
    sys::write_str(&format!(
        "USBD:KEY usage={:#x} {} t={}\n",
        edge.usage,
        if edge.pressed { "down" } else { "up" },
        sys::clock()
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

/// A report-protocol pointer's output: a position (`ABS_MOTION`, packed
/// `x | y << 16`, both `0..=0xFFFF`) or what a boot mouse would send.
fn report_out(out: Out) -> SourceRecord {
    match out {
        Out::Position { x, y } => SourceRecord {
            kind: raw_kind::ABS_MOTION,
            code: 0,
            value: (u32::from(x) | u32::from(y) << 16) as i32,
        },
        Out::Mouse(out) => pointer(out),
    }
}

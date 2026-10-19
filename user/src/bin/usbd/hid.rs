//! A configured keyboard, mouse or tablet, publishing onto the raw input bus
//! through its own kernel source (`docs/usb-hid-plan.md` U1, U4). Boot
//! keyboards and mice are decoded by their fixed boot reports; a
//! report-protocol pointer (a tablet) by the layout its report descriptor
//! gave, as absolute positions (`ABS_MOTION`, `0..=0xFFFF`) or, for a
//! relative one, motion. A gamepad (`docs/usb-gamepad-plan.md` G2) is read
//! the same way, through the layout `usbhid::report::gamepad` placed: changed
//! axes and button edges, one `DEVICE` identity record at the start.
//!
//! Each HID interface is one source of its class, so the kernel stamps a
//! device id of its own and refuses records the class may not carry. The
//! driver holds no input policy: reports become edges and deltas, nothing
//! more (no cursor, no clamping, no repeat).

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use user::trace;

use usbhid::boot::{parse_mouse, BootKeyboard, BootMouse, KeyEdge, MouseOut};
use usbhid::busrec::{self, BusRecord};
use usbhid::desc::Protocol;
use usbhid::report::gamepad::{self, Gamepad, PadOut};
use usbhid::report::{self, Out, Pointer};
use user::sys::{self, raw_kind, source_class, SourceRecord, SOURCE_MAX_BATCH};

use super::Error;

// The record kinds `usbhid::busrec` repeats must be the bus's own.
const _: () = {
    assert!(busrec::BUTTON == raw_kind::BUTTON);
    assert!(busrec::AXIS == raw_kind::AXIS);
    assert!(busrec::DEVICE == raw_kind::DEVICE);
};

/// What a report-protocol interface's descriptor turned out to be.
pub(super) enum ReportLayout {
    Pointer(Pointer),
    /// Boxed: a pad's layout is several times a pointer's.
    Pad(Box<Gamepad>),
}

/// The decoder for one interface's reports.
enum Decoder {
    Keyboard(BootKeyboard),
    Mouse(BootMouse),
    Report(report::Decoder),
    Pad(Box<gamepad::Decoder>),
}

pub(super) struct Hid {
    source: u64,
    decoder: Decoder,
    /// Records the kernel refused or throttled (it counts them too).
    pub(super) refused: u64,
}

impl Hid {
    /// Register a source for a boot `protocol` interface, or for the
    /// report-protocol pointer or gamepad `layout` describes.
    pub(super) fn new(protocol: Protocol, layout: Option<ReportLayout>) -> Result<Hid, Error> {
        let (class, decoder) = match (protocol, layout) {
            (Protocol::Keyboard, _) => (
                source_class::KEYBOARD,
                Decoder::Keyboard(BootKeyboard::new()),
            ),
            // A boot mouse left in report protocol (`class::wheel_layout`)
            // is decoded by its descriptor, like a tablet.
            (Protocol::Mouse, None) => (source_class::POINTER, Decoder::Mouse(BootMouse::new())),
            (Protocol::None, Some(ReportLayout::Pad(layout))) => (
                source_class::GAMEPAD,
                Decoder::Pad(Box::new(gamepad::Decoder::new(*layout))),
            ),
            // A pad never rides a boot protocol; a pad layout on a boot
            // interface is not something `class.rs` produces.
            (_, Some(ReportLayout::Pad(_))) => {
                return Err(Error::Descriptor("gamepad on a boot interface"))
            }
            (Protocol::Mouse | Protocol::None, Some(ReportLayout::Pointer(layout))) => {
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
        if trace && trace::enabled(trace::Channel::Usbd) {
            let hex: alloc::string::String = report.iter().map(|b| format!("{b:02x}")).collect();
            trace::line!(trace::Channel::Usbd, "REPORT {hex}");
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
            // A report the layout refuses (another id, too short) emits
            // nothing and changes nothing.
            Decoder::Pad(decoder) => {
                decoder.feed(report, |out| {
                    if trace {
                        trace_pad(&out);
                    }
                    records.push(pad(out));
                });
            }
        }
        self.publish(&records);
        records
            .iter()
            .any(|r| r.kind == raw_kind::KEY && r.value == 1)
    }

    /// Publish the device's identity (`bcdDevice` included), once, right
    /// after the source is registered. Diagnostics and a later mapping table
    /// read it; nothing treats it as a classification.
    pub(super) fn announce(&mut self, vendor: u16, product: u16, version: u16) {
        let record = busrec::device_record(vendor, product, version);
        self.publish(&[source_record(record)]);
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
            // Sticks back to rest, hat centred, buttons released: published
            // before the source closes (the kernel does the same on close).
            Decoder::Pad(decoder) => decoder.release_all(|out| {
                if trace {
                    trace_pad(&out);
                }
                records.push(pad(out));
            }),
        }
        self.publish(&records);
        let _ = sys::input_source_close(self.source);
    }
}

/// The harness's evidence for one key edge `usbd` published.
/// `t=` is the tick (10 ms) it was published, so a harness can tell `usbd`'s
/// delivery latency from `inputd`'s (which stamps its own lines the same way).
fn trace_key(edge: &KeyEdge) {
    trace::line!(
        trace::Channel::Usbd,
        "KEY usage={:#x} {} t={}",
        edge.usage,
        if edge.pressed { "down" } else { "up" },
        sys::clock()
    );
}

/// The harness's evidence for one gamepad change `usbd` published, with the
/// tick like [`trace_key`].
fn trace_pad(out: &PadOut) {
    match *out {
        PadOut::Axis { usage, value } => trace::line!(
            trace::Channel::Usbd,
            "PAD axis usage={usage:#x} value={value} t={}",
            sys::clock()
        ),
        PadOut::Button { usage, pressed } => trace::line!(
            trace::Channel::Usbd,
            "PAD button usage={usage} {} t={}",
            if pressed { "down" } else { "up" },
            sys::clock()
        ),
    }
}

fn source_record(record: BusRecord) -> SourceRecord {
    SourceRecord {
        kind: record.kind,
        code: record.code,
        value: record.value,
    }
}

/// Bus gamepad records (`AXIS`, `BUTTON`), mapped by `usbhid::busrec`.
fn pad(out: PadOut) -> SourceRecord {
    source_record(busrec::pad_record(out))
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

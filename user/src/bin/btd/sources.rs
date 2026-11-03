//! The input sources of `btd`'s devices (docs/bluetooth-plan.md decision 2):
//! one kernel source per half of a connected device, of the class its bond's
//! record gave. The host decides; this registers, publishes and closes, with
//! the same record mapping `usbd` uses (`usbd/hid.rs`), so a Bluetooth key
//! and a USB key are the same record on the bus.
//!
//! Closing a source makes the kernel release anything it still holds; the
//! host has already published the releases, so both views agree.

use alloc::vec::Vec;
use bthid::decode::Event;
use bthost::SourceKind;
use usbhid::boot::{KeyEdge, MouseOut};
use usbhid::report::Out;
use user::sys::{self, raw_kind, source_class, SourceRecord};
use user::trace::{self, Channel};

struct Open {
    handle: u16,
    kind: SourceKind,
    id: u64,
}

pub(super) struct Sources {
    open: Vec<Open>,
    /// Harness builds (`trace=1`): echo each key edge for the judge.
    trace_keys: bool,
    /// The crash-test build's first attempt: die holding the first key.
    crash_on_key: bool,
}

impl Sources {
    pub(super) fn new(trace_keys: bool, crash_on_key: bool) -> Sources {
        Sources {
            open: Vec::new(),
            trace_keys,
            crash_on_key,
        }
    }

    pub(super) fn open(&mut self, handle: u16, kind: SourceKind) {
        let class = match kind {
            SourceKind::Keyboard => source_class::KEYBOARD,
            SourceKind::Pointer => source_class::POINTER,
            SourceKind::Tablet => source_class::TABLET,
        };
        match sys::input_source_register(class) {
            Ok(id) => self.open.push(Open { handle, kind, id }),
            Err(errno) => trace::fail!(
                Channel::Bt,
                "SOURCE",
                "register class={class} errno={errno}"
            ),
        }
    }

    pub(super) fn publish(&mut self, handle: u16, event: Event) {
        let (wants_keys, record) = match event {
            Event::Key(edge) => {
                if self.trace_keys {
                    trace_key(&edge);
                }
                (true, key(edge))
            }
            Event::Pointer(out) => (false, pointer_record(out)),
        };
        let source = self
            .open
            .iter()
            .find(|s| s.handle == handle && (s.kind == SourceKind::Keyboard) == wants_keys);
        if let Some(source) = source {
            // The kernel counts what it throttles or refuses.
            let _ = sys::input_source_publish(source.id, &[record]);
            if self.crash_on_key && record.kind == raw_kind::KEY && record.value == 1 {
                // `LAZYOS_BT_CRASH_TEST`: die with the key down, so the
                // harness can see the kernel release it.
                trace::line!(Channel::Bt, "CRASH exiting with a key held");
                sys::exit(3);
            }
        }
    }

    /// Close every source of `handle`.
    pub(super) fn close(&mut self, handle: u16) {
        self.open.retain(|s| {
            if s.handle == handle {
                let _ = sys::input_source_close(s.id);
                false
            } else {
                true
            }
        });
    }

    /// The controller went away: every device with it.
    pub(super) fn close_all(&mut self) {
        for source in self.open.drain(..) {
            let _ = sys::input_source_close(source.id);
        }
    }
}

fn trace_key(edge: &KeyEdge) {
    trace::line!(
        Channel::Bt,
        "KEY usage={:#x} {} t={}",
        edge.usage,
        if edge.pressed { "down" } else { "up" },
        sys::clock()
    );
}

fn key(edge: KeyEdge) -> SourceRecord {
    SourceRecord {
        kind: raw_kind::KEY,
        code: edge.usage,
        value: i32::from(edge.pressed),
    }
}

/// HID axes already match the bus (`dy > 0` down, wheel > 0 up).
fn mouse_record(out: MouseOut) -> SourceRecord {
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

fn pointer_record(out: Out) -> SourceRecord {
    match out {
        Out::Position { x, y } => SourceRecord {
            kind: raw_kind::ABS_MOTION,
            code: 0,
            value: (u32::from(x) | u32::from(y) << 16) as i32,
        },
        Out::Mouse(out) => mouse_record(out),
    }
}

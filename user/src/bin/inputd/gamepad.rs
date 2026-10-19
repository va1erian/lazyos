//! The gamepad half of `inputd` (`docs/usb-gamepad-plan.md`, phase G3).
//!
//! The decisions live in `inputmap::Pads` (host-tested and fuzzed): who is a
//! pad, from the kernel's `SOURCE` records and source table, and what a pad is
//! doing. This module feeds it the bus, answers `AttachGamepadState`, writes
//! the focused session's page and delivers `GamepadAxis`/`GamepadButton` to
//! the focused session only, like the keyboard. Focus arrival sends
//! `GamepadEnter` (the page is live by then); focus loss clears the page and
//! then sends `GamepadLeave`, and a session whose events were dropped gets a
//! Leave and an Enter (`delivery.rs`) so it resets.
//!
//! Known gaps (not applied to pad events): the click barrier (`settle.rs`)
//! and the panel-menu hold (`held.rs`), which are about keys.
//!
//! Serial: `INPUTD:PAD attach|detach|refused device=<id>`, `INPUTD:PAD
//! device=<id> vendor=.. product=.. version=..` (a `DEVICE` record),
//! `INPUTD:PAD page session=<id>`, `INPUTD:PAD resync pads=<n> rejected=<n>`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use inputmap::gamepad::page::{self, SharedPads, Snapshot, Writer};
use inputmap::gamepad::route::{Route, SourceEntry};
use inputmap::{Notice, PadOut, Pads, Pointer, PointerOut, RawPad, RawPointer};
use user::messenger::input::wire;
use user::messenger::{Message, Result};
use user::sys;
use user::trace;

use super::hub::Hub;

/// Registrations and unplugs are rare, but a driver that churns must not
/// flood the console: at most this many `PAD` lines per window (10 ms ticks).
static LINES: tracefmt::Budget = tracefmt::Budget::new(24, 100);

macro_rules! pad_line {
    ($($text:tt)+) => {
        if user::trace::within(&LINES, trace::Channel::Inputd) {
            trace::line!(trace::Channel::Inputd, $($text)+);
        }
    };
}

/// One attached page.
struct Page {
    /// The buffer handle in this task, closed when the page goes.
    handle: u64,
    /// Where it is mapped here.
    va: u64,
    writer: Writer,
}

impl Page {
    fn close(self) {
        let _ = sys::buffer_close(self.handle);
    }
}

/// The page mapped at `va`.
fn shared_at<'a>(va: u64) -> &'a SharedPads {
    // SAFETY: `va` is this task's mapping of a live shared buffer at least
    // `page::SIZE` bytes long (checked on attach), 8-byte aligned (checked
    // too; mappings are page-aligned), and it stays mapped until the page is
    // closed, which only happens after its last use here. The fields are
    // atomics, so a client writing the same memory concurrently races on
    // atomics only, never undefined behaviour here.
    unsafe { &*(va as *const SharedPads) }
}

/// The pads, the sessions' pages and the events waiting to go out.
#[derive(Default)]
pub(super) struct Gamepads {
    pads: Pads,
    /// Events produced this pass, for the focused session.
    pub(super) out: Vec<PadOut>,
    notices: Vec<Notice>,
    pages: BTreeMap<u64, Page>,
}

impl Gamepads {
    /// Send a pointer-kind record where its device's class says: the cursor,
    /// the pad it belongs to, or nowhere.
    pub(super) fn route(
        &mut self,
        record: RawPointer,
        cursor: &mut Pointer,
        outputs: &mut Vec<PointerOut>,
    ) {
        match self.pads.route(record.device, record.kind) {
            Route::Pointer => cursor.apply(record, outputs),
            Route::Pad => {
                self.pads.feed(RawPad::from(record), &mut self.out);
            }
            Route::Drop => {}
        }
    }

    /// An `AXIS` record: a pad's, else nobody's.
    pub(super) fn axis(&mut self, record: RawPad) {
        if self.pads.route(record.device, record.kind) == Route::Pad {
            self.pads.feed(record, &mut self.out);
        }
    }

    /// The kernel's `SOURCE` record.
    pub(super) fn source(&mut self, record: RawPad) {
        self.pads.source(record, &mut self.out, &mut self.notices);
        self.trace_notices();
    }

    /// A `DEVICE` record, for the log.
    pub(super) fn device(&self, device: u8, vendor: u16, info: u32) {
        pad_line!(
            "PAD device={device} vendor={vendor:#06x} product={:#06x} version={:#06x} pad={}",
            info & 0xFFFF,
            info >> 16,
            u8::from(self.pads.classifier().is_pad(device))
        );
    }

    /// Read the kernel's source table, the truth about who is a pad: when the
    /// bus opens and after every `Dropped`, since a `SOURCE` record can be
    /// evicted from the ring like any other. A kernel that refuses it leaves
    /// what is known (and unknown devices keep today's pointer routing).
    pub(super) fn read_table(&mut self, ts_ns: u64, seq: u64) {
        let mut table = [sys::SourceInfo::default(); sys::MAX_SOURCES];
        match sys::input_raw_sources(&mut table) {
            Ok(count) => {
                let entries: Vec<SourceEntry> = table[..count]
                    .iter()
                    .map(|info| SourceEntry {
                        device: info.device,
                        class: info.class,
                        generation: info.generation,
                    })
                    .collect();
                self.pads
                    .table(&entries, ts_ns, seq, &mut self.out, &mut self.notices);
                self.trace_notices();
            }
            Err(code) => pad_line!("PAD table failed code={code}"),
        }
    }

    /// The bus lost records: release and centre the pads, then find out which
    /// sources are still there.
    pub(super) fn dropped(&mut self, ts_ns: u64, seq: u64) {
        self.pads.resync(ts_ns, seq, &mut self.out);
        self.read_table(ts_ns, seq);
        pad_line!(
            "PAD resync pads={} rejected={}",
            self.pads.engine().count(),
            self.pads.rejected()
        );
    }

    /// The end of a bus drain: coalesced axis changes join `out`.
    pub(super) fn flush(&mut self) {
        self.pads.flush(&mut self.out);
    }

    fn trace_notices(&mut self) {
        for notice in self.notices.drain(..) {
            match notice {
                Notice::Attached(device) => pad_line!("PAD attach device={device}"),
                Notice::Refused(device) => pad_line!("PAD refused device={device}"),
                Notice::Detached(device) => pad_line!("PAD detach device={device}"),
            }
        }
    }

    /// Bring every page up to date: the focused session's shows the pads now,
    /// every other one is cleared. Cheap when nothing changed.
    fn publish(&mut self, focused: Option<u64>) {
        for (session, page) in self.pages.iter_mut() {
            let state = Snapshot::of(self.pads.engine(), focused == Some(*session));
            page.writer.publish(shared_at(page.va), state);
        }
    }

    /// Clear `session`'s page now (it is about to get `GamepadLeave`).
    fn clear(&mut self, session: u64) {
        if let Some(page) = self.pages.get_mut(&session) {
            let state = Snapshot::of(self.pads.engine(), false);
            page.writer.publish(shared_at(page.va), state);
        }
    }

    /// Drop `session`'s page, if it had one.
    pub(super) fn forget(&mut self, session: u64) {
        if let Some(page) = self.pages.remove(&session) {
            page.close();
        }
    }
}

impl Hub {
    /// `AttachGamepadState`: adopt the page the caller's own session carried.
    pub(super) fn attach_gamepad_state(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = message.decode(wire::decode_attach_gamepad_state_args)?;
        let result = self.attach_gamepad_state_inner(message, &args);
        if result.is_err() {
            // Decoded, so the page is ours to close.
            let _ = sys::buffer_close(args.state.handle);
        }
        result
    }

    fn attach_gamepad_state_inner(
        &mut self,
        message: &Message,
        args: &wire::AttachGamepadStateArgs,
    ) -> Result<Vec<u8>> {
        let va = self.map_page(message, args.session, &args.state, page::SIZE)?;
        self.gamepads.forget(args.session);
        self.gamepads.pages.insert(
            args.session,
            Page {
                handle: args.state.handle,
                va,
                writer: Writer::new(),
            },
        );
        pad_line!("PAD page session={}", args.session);
        // Seed it at once, so a focused session sees its pads right away.
        self.publish_pad_pages();
        Ok(Vec::new())
    }

    /// Bring the pad pages up to date (after each pass and each focus change).
    pub(super) fn publish_pad_pages(&mut self) {
        let focused = self.router.focused_session();
        self.gamepads.publish(focused);
    }

    /// Send this pass's pad events to the focused session, if there is one:
    /// nobody else ever sees a pad.
    pub(super) fn deliver_pads(&mut self) {
        if self.gamepads.out.is_empty() {
            return;
        }
        let mut events = core::mem::take(&mut self.gamepads.out);
        if let Some(session) = self.router.focused_session() {
            for event in &events {
                let (method, body) = encode_pad(event);
                self.send(session, method, body);
            }
        }
        events.clear();
        self.gamepads.out = events;
    }

    /// `session` gained the keyboard: its page goes live, then it is told.
    pub(super) fn pad_enter(&mut self, session: u64) {
        self.publish_pad_pages();
        self.send(session, wire::METHOD_GAMEPADENTER, Ok(Vec::new()));
    }

    /// `session` lost the keyboard: its page is cleared, then it is told, so
    /// a client that reads the page on `GamepadLeave` finds it empty.
    pub(super) fn pad_leave(&mut self, session: u64) {
        self.gamepads.clear(session);
        self.send(session, wire::METHOD_GAMEPADLEAVE, Ok(Vec::new()));
    }
}

/// The method and body that carry `event`.
fn encode_pad(event: &PadOut) -> (u32, super::hub::Encoded) {
    match *event {
        PadOut::Axis {
            device,
            code,
            value,
            ts_ns,
            seq,
        } => (
            wire::METHOD_GAMEPADAXIS,
            wire::encode_gamepad_axis_args(&wire::GamepadAxisArgs {
                device: u32::from(device),
                code: u32::from(code),
                value: i32::from(value),
                ts_ns,
                seq,
            }),
        ),
        PadOut::Button {
            device,
            code,
            pressed,
            ts_ns,
            seq,
        } => (
            wire::METHOD_GAMEPADBUTTON,
            wire::encode_gamepad_button_args(&wire::GamepadButtonArgs {
                device: u32::from(device),
                code: u32::from(code),
                pressed,
                ts_ns,
                seq,
            }),
        ),
    }
}

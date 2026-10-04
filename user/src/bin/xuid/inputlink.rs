//! `xuid` as an `inputd` shell client (`docs/input-plan.md`).
//!
//! The compositor no longer carries keystrokes for clients that use the input
//! service: it tells `inputd` which task created each surface and which one has
//! focus, and `inputd` delivers keys straight to the focused client. `xuid`
//! keeps synthesising the frozen `KeyDown`/`KeyUp` only for legacy surfaces
//! (those without an `inputd` session), and keeps its own hotkeys.
//!
//! It also takes the pointer from `inputd` once attached (`pointer_feed.rs`):
//! one cursor for every pointing device, clamped to the bounds sent here.
//!
//! The link is best-effort: `inputd` starts after the compositor, so it is
//! connected lazily and every failure just drops it to be retried; a session
//! that cannot register simply stays on legacy delivery.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use user::messenger::input::{PointerState, ShellEvent, ShellLink};
use user::messenger::{errno, Error};
use user::sys;

use super::compositor::Compositor;
use super::pointer_feed::{forwarded, supersedes, MAX_EVENTS};

/// Ticks (100 Hz) between attempts to reach `inputd`.
const RETRY_TICKS: u64 = 100;

pub(super) struct InputLink {
    link: Option<ShellLink>,
    next_try: u64,
    /// Surfaces `inputd` has been told about.
    registered: BTreeSet<u64>,
    /// The focus `inputd` last heard (`None`: never told).
    told_focus: Option<Option<u64>>,
    /// The last failure logged, so a changing error is reported but retries
    /// of the same one stay quiet.
    reported: Option<Option<i64>>,
    /// The pointer comes from `inputd`, not the kernel's display stream.
    pub(super) owns_pointer: bool,
    /// The forwarded buttons `inputd` last reported held.
    pub(super) buttons: u32,
}

impl InputLink {
    pub(super) fn new() -> InputLink {
        InputLink {
            link: None,
            next_try: 0,
            registered: BTreeSet::new(),
            told_focus: None,
            reported: None,
            owns_pointer: false,
            buttons: 0,
        }
    }
}

impl Compositor {
    /// The endpoint `inputd` posts shell events (pointer moves, sessions) to,
    /// while a link exists: the main loop parks on it beside its requests.
    pub(super) fn input_events(&self) -> Option<user::messenger::Endpoint> {
        self.input.link.as_ref().map(|link| link.events_endpoint())
    }

    /// Bring `inputd` up to date with the surface table and focus, and apply
    /// what it reported. Cheap when nothing changed; called once per loop pass
    /// and when a surface is created (so the client's `Open` can find it).
    pub(super) fn sync_input(&mut self) {
        if self.input.link.is_none() && !self.connect_input() {
            return;
        }
        if !self.apply_input_events() || !self.push_input_state() {
            self.drop_input_link();
        }
    }

    /// Try to attach to `inputd`; whether a link now exists.
    fn connect_input(&mut self) -> bool {
        if sys::clock() < self.input.next_try {
            return false;
        }
        match ShellLink::connect() {
            Ok(link) => {
                self.input.link = Some(link);
                self.input.registered.clear();
                self.input.told_focus = None;
                sys::write_str("xuid: attached to inputd\n");
                self.register_chords();
                self.adopt_inputd_pointer();
                true
            }
            Err(error) => {
                // Say why once; `inputd` legitimately starts after the
                // compositor, so the retries stay quiet.
                let code = error.errno();
                if self.input.reported != Some(code) {
                    self.input.reported = Some(code);
                    sys::write_str(&alloc::format!(
                        "xuid: inputd not reachable yet ({code:?})
"
                    ));
                }
                self.input.next_try = sys::clock() + RETRY_TICKS;
                false
            }
        }
    }

    /// Register the chords `keys.rs` acts on itself, so `inputd` keeps them
    /// from session clients (the compositor still sees them on the kernel
    /// stream). Plain Escape stays with the client.
    fn register_chords(&mut self) {
        // HID usages: Tab, F4, Escape; modifier bits from `inputmap::mods`.
        const CHORDS: [(u32, u32); 4] = [(0x2B, 4), (0x2B, 2), (0x3D, 4), (0x29, 2)];
        let Some(link) = self.input.link.as_ref() else {
            return;
        };
        for (code, mods) in CHORDS {
            let _ = link.register_hotkey(code, mods);
        }
    }

    /// Hand the pointer to `inputd`: give it the screen bounds and seed the
    /// cursor from it. On failure the kernel stream stays the source.
    fn adopt_inputd_pointer(&mut self) {
        let (width, height) = (self.screen.width() as u32, self.screen.height() as u32);
        let Some(link) = self.input.link.as_ref() else {
            return;
        };
        let seed = link
            .set_bounds(width, height)
            .and_then(|()| link.get_pointer());
        let Ok(seed) = seed else {
            sys::write_str("xuid: pointer stays on the kernel stream\n");
            return;
        };
        self.input.owns_pointer = true;
        self.apply_pointer(&seed);
        sys::write_str("xuid: pointer from inputd\n");
    }

    /// Apply queued `inputd` events. `false` when the link is dead.
    ///
    /// Pointer events are coalesced (docs/performance-plan.md P3.4): a state
    /// that only moves the pointer is replaced by the next one when that
    /// keeps the same buttons ([`supersedes`]), so a backlog that built up
    /// while the compositor was busy costs one repaint, not one per event.
    /// Button edges and wheel notches are never merged away.
    fn apply_input_events(&mut self) -> bool {
        let mut pending: Option<PointerState> = None;
        // The forwarded buttons held before `pending`.
        let mut before = self.input.buttons;
        let alive = loop {
            if !self.held.is_empty() && self.held.room() < MAX_EVENTS {
                // An animation filled the held queue: leave the rest queued
                // in `inputd` until the main loop has handled it.
                break true;
            }
            let Some(link) = self.input.link.as_mut() else {
                break false;
            };
            match link.poll_event() {
                Ok(Some(ShellEvent::Pointer(state))) if self.input.owns_pointer => {
                    if let Some(old) = pending {
                        if !supersedes(before, &old, &state) {
                            self.apply_pointer(&old);
                            before = forwarded(old.buttons);
                        }
                    }
                    pending = Some(state);
                }
                Ok(Some(ShellEvent::SessionOpened(surface))) => self.set_session(surface, true),
                Ok(Some(ShellEvent::SessionClosed(surface))) => self.set_session(surface, false),
                // Hotkeys, grants and the escape chord are not used yet: the
                // compositor keeps its own hotkey table until they are.
                Ok(Some(_)) => {}
                Ok(None) => break true,
                Err(_) => break false,
            }
        };
        if let Some(state) = pending {
            self.apply_pointer(&state);
        }
        alive
    }

    /// During an animation frame: hold the pointer events `inputd` queued,
    /// in order, so the cursor keeps moving (`held.rs`). Session changes are
    /// applied at once; they only flip a delivery flag. A dead link is left
    /// for [`Compositor::sync_input`] to notice.
    pub(super) fn hold_inputd_pointer(&mut self) {
        while self.held.room() >= MAX_EVENTS {
            let Some(link) = self.input.link.as_mut() else {
                return;
            };
            match link.poll_event() {
                Ok(Some(ShellEvent::SessionOpened(surface))) => self.set_session(surface, true),
                Ok(Some(ShellEvent::SessionClosed(surface))) => self.set_session(surface, false),
                Ok(Some(ShellEvent::Pointer(state))) => {
                    let (events, count) = self.translate_pointer(&state);
                    for event in &events[..count] {
                        self.held.push(*event);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return,
            }
        }
    }

    fn set_session(&mut self, surface: u64, open: bool) {
        if let Some(found) = self.surfaces.iter_mut().find(|s| s.id == surface) {
            found.input_session = open;
        }
    }

    /// Register new surfaces, forget destroyed ones and report focus, as
    /// one-way notes (docs/performance-plan.md P3.6): the compositor never
    /// waits on `inputd` here, where a two-way call could stall the cursor
    /// for up to its 200 ms timeout. `inputd` handles one sender's requests
    /// in order on the endpoint the clients' `Open` also arrives on, so a
    /// surface noted when it is created is known before its client can open
    /// a session. `false` when `inputd` is gone. A note that found the queue
    /// full is not a dead link: it stays undone and is retried on the next
    /// pass, since dropping the link would flip every window to legacy keys
    /// and back, and the keys typed across that switch went to whichever
    /// side had just let go.
    fn push_input_state(&mut self) -> bool {
        let Some(link) = self.input.link.as_ref() else {
            return false;
        };
        for surface in self.surfaces.iter().filter(|s| s.is_window()) {
            if !self.input.registered.contains(&surface.id) {
                match sent(link.note_surface(surface.id, surface.owner)) {
                    Some(true) => {
                        self.input.registered.insert(surface.id);
                    }
                    Some(false) => {}
                    None => return false,
                }
            }
        }
        let gone: Vec<u64> = self
            .input
            .registered
            .iter()
            .copied()
            .filter(|id| !self.surfaces.iter().any(|s| s.id == *id))
            .collect();
        for id in gone {
            match sent(link.forget_surface(id)) {
                Some(true) => {
                    self.input.registered.remove(&id);
                }
                Some(false) => {}
                None => return false,
            }
        }
        if self.input.told_focus != Some(self.focused) {
            match sent(link.note_focus(self.focused)) {
                Some(true) => self.input.told_focus = Some(self.focused),
                Some(false) => {}
                None => return false,
            }
        }
        true
    }

    /// `inputd` went away: forget the link (retried later) and fall back to
    /// legacy key delivery for every surface and the kernel pointer.
    fn drop_input_link(&mut self) {
        if let Some(link) = self.input.link.take() {
            link.close();
        }
        if self.input.owns_pointer {
            self.release_pointer();
            self.input.owns_pointer = false;
            sys::write_str("xuid: pointer back on the kernel stream\n");
        }
        self.input.next_try = sys::clock() + RETRY_TICKS;
        for surface in self.surfaces.iter_mut() {
            surface.input_session = false;
        }
    }
}

/// How a one-way note went: `Some(true)` queued, `Some(false)` the queue
/// was full (retry later, the link is fine), `None` the link is dead.
fn sent(result: Result<(), Error>) -> Option<bool> {
    match result {
        Ok(()) => Some(true),
        Err(Error::Errno(code)) if code == -errno::EAGAIN => Some(false),
        Err(_) => None,
    }
}

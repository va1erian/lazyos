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

use user::messenger::input::{ShellEvent, ShellLink};
use user::sys;

use super::compositor::Compositor;

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
    fn apply_input_events(&mut self) -> bool {
        loop {
            let Some(link) = self.input.link.as_mut() else {
                return false;
            };
            match link.poll_event() {
                Ok(Some(ShellEvent::SessionOpened(surface))) => self.set_session(surface, true),
                Ok(Some(ShellEvent::SessionClosed(surface))) => self.set_session(surface, false),
                Ok(Some(ShellEvent::Pointer(state))) if self.input.owns_pointer => {
                    self.apply_pointer(&state)
                }
                // Hotkeys, grants and the escape chord are not used yet: the
                // compositor keeps its own hotkey table until they are.
                Ok(Some(_)) => {}
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }

    fn set_session(&mut self, surface: u64, open: bool) {
        if let Some(found) = self.surfaces.iter_mut().find(|s| s.id == surface) {
            found.input_session = open;
        }
    }

    /// Register new surfaces, forget destroyed ones and report focus.
    /// `false` when `inputd` stopped answering.
    fn push_input_state(&mut self) -> bool {
        let Some(link) = self.input.link.as_ref() else {
            return false;
        };
        for surface in self.surfaces.iter().filter(|s| !s.desktop) {
            if !self.input.registered.contains(&surface.id) {
                if link.register_surface(surface.id, surface.owner).is_err() {
                    return false;
                }
                self.input.registered.insert(surface.id);
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
            if link.unregister_surface(id).is_err() {
                return false;
            }
            self.input.registered.remove(&id);
        }
        if self.input.told_focus != Some(self.focused) {
            if link.set_focus(self.focused).is_err() {
                return false;
            }
            self.input.told_focus = Some(self.focused);
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

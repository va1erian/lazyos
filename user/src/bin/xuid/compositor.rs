//! The [`Compositor`]: every piece of mutable session state `xuid` owns
//! (issue #360), so the input, request, drag & drop, shell and paint paths are
//! methods on one value instead of free functions threading a dozen `&mut`
//! parameters. Invariants such as "focus changed, so tell the shell and
//! repaint" live in one place per method rather than at every call site.

use alloc::vec::Vec;
use user::messenger::display::{Canvas, Rect};

use super::clock::{self, Clock};
use super::drag::DragSession;
use super::inputlink::InputLink;
use super::origin::OpenHint;
use super::resize::ResizeDrag;
use super::shell::{taskbar_visible, AltTab, Modifiers, ShellSub};
use super::surface::{Drag, Surface};
use super::theme::TASKBAR_H;
use super::powerfeed::PowerFeed;
use super::themefeed::ThemeFeed;

/// The session compositor's whole mutable state.
pub(super) struct Compositor {
    /// The mapped screen buffer every frame is composed into.
    pub(super) screen: Canvas,
    /// Every live surface; the vector *is* the z-order (tail paints last).
    pub(super) surfaces: Vec<Surface>,
    /// The pointer position, screen-absolute.
    pub(super) pointer: (i32, i32),
    /// The focused window, if any.
    pub(super) focused: Option<u64>,
    /// The window-manager title-bar drag (issue #143).
    pub(super) drag: Option<Drag>,
    /// The live interactive edge resize, if any.
    pub(super) resize: Option<ResizeDrag>,
    /// The last title-bar press, for the double-click-to-maximize rule:
    /// `(surface id, PIT tick, pointer)`.
    pub(super) last_title_click: Option<(u64, u64, (i32, i32))>,
    /// The live drag & drop session, if any (issue #145).
    pub(super) drag_session: Option<DragSession>,
    /// Whether a pointer button is held (`DragStart` requires it).
    pub(super) button_down: bool,
    /// Buttons whose press the compositor consumed (taskbar, window buttons,
    /// title bar, desktop): their release is swallowed too, so no surface sees
    /// an unmatched `POINTER_UP` even if focus moved in between.
    pub(super) consumed: u32,
    /// The next surface id to hand out.
    pub(super) next_id: u64,
    /// The registered shell subscriber (issue #167).
    pub(super) shell: Option<ShellSub>,
    /// The held modifier keys.
    pub(super) mods: Modifiers,
    /// The open Alt+Tab overlay.
    pub(super) alt_tab: Option<AltTab>,
    /// One event-encode buffer for the whole life of the compositor: the user
    /// bump allocator never reclaims, so events reuse it instead of
    /// allocating per message.
    pub(super) scratch: Vec<u8>,
    /// The taskbar clock (issue #370).
    pub(super) clock: Clock,
    /// The live `sys/ui/*` theme follower.
    pub(super) themefeed: ThemeFeed,
    /// `init`'s shutdown progress (the shutting-down overlay).
    pub(super) powerfeed: PowerFeed,
    /// The compositor's side of `inputd` (`docs/input-plan.md`).
    pub(super) input: InputLink,
    /// Pending open-origin hints, at most one per task.
    pub(super) hints: Vec<OpenHint>,
    /// PIT tick at which the next client liveness probe is due.
    pub(super) next_probe: u64,
}

impl Compositor {
    /// A compositor over `screen` with no surfaces and the pointer centered.
    pub(super) fn new(screen: Canvas) -> Compositor {
        let pointer = (screen.width() / 2, screen.height() / 2);
        Compositor {
            screen,
            surfaces: Vec::new(),
            pointer,
            focused: None,
            drag: None,
            resize: None,
            last_title_click: None,
            drag_session: None,
            button_down: false,
            consumed: 0,
            next_id: 1,
            shell: None,
            mods: Modifiers::default(),
            alt_tab: None,
            scratch: Vec::with_capacity(64),
            clock: Clock::new(),
            themefeed: ThemeFeed::new(),
            powerfeed: PowerFeed::new(),
            input: InputLink::new(),
            hints: Vec::new(),
            next_probe: 0,
        }
    }

    /// The whole screen as a rectangle.
    pub(super) fn full(&self) -> Rect {
        Rect::new(0, 0, self.screen.width(), self.screen.height())
    }

    /// Whether the built-in fallback taskbar paints (no `"shell"` subscriber).
    pub(super) fn taskbar(&self) -> bool {
        taskbar_visible(self.shell.as_ref())
    }

    /// The rectangle windows may occupy: the whole screen, less the fallback
    /// taskbar strip while it is visible. The single source of the rule that
    /// `GetWorkArea`, window movement and maximize all share.
    pub(super) fn work_area(&self) -> Rect {
        let height = if self.taskbar() {
            (self.screen.height() - TASKBAR_H).max(0)
        } else {
            self.screen.height()
        };
        Rect::new(0, 0, self.screen.width().max(0), height)
    }

    /// Advance the taskbar clock and repaint just its rectangle when the
    /// minute (or zone) changed and the built-in bar is showing.
    pub(super) fn tick_clock(&mut self) {
        if self.clock.poll() && self.taskbar() {
            let dims = (self.screen.width(), self.screen.height());
            self.repaint(clock::rect(dims));
        }
    }

    /// Follow the confd theme and repaint the whole screen when it changed.
    pub(super) fn tick_theme(&mut self) {
        if self.themefeed.poll() {
            self.repaint_full();
        }
    }

    /// Follow `init`'s shutdown and repaint everything when the overlay rises.
    pub(super) fn tick_power(&mut self) {
        if self.powerfeed.poll() {
            self.repaint_full();
        }
    }

    /// Repaint the full screen.
    pub(super) fn repaint_full(&mut self) {
        let full = self.full();
        self.repaint(full);
    }
}

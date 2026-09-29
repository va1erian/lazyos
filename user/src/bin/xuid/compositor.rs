//! The [`Compositor`]: every piece of mutable session state `xuid` owns
//! (issue #360), so the input, request, drag & drop, shell and paint paths are
//! methods on one value instead of free functions threading a dozen `&mut`
//! parameters. Invariants such as "focus changed, so tell the shell and
//! repaint" live in one place per method rather than at every call site.

use alloc::vec::Vec;
use user::messenger::display::{Canvas, Rect};

use super::drag::DragSession;
use super::shell::{taskbar_visible, AltTab, Modifiers, ShellSub};
use super::surface::{Drag, Surface};

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
            drag_session: None,
            button_down: false,
            consumed: 0,
            next_id: 1,
            shell: None,
            mods: Modifiers::default(),
            alt_tab: None,
            scratch: Vec::with_capacity(64),
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

    /// Repaint the full screen.
    pub(super) fn repaint_full(&mut self) {
        let full = self.full();
        self.repaint(full);
    }
}

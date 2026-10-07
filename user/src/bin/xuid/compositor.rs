//! The [`Compositor`]: every piece of mutable session state `xuid` owns
//! (issue #360), so the input, request, drag & drop, shell and paint paths are
//! methods on one value instead of free functions threading a dozen `&mut`
//! parameters. Invariants such as "focus changed, so tell the shell and
//! repaint" live in one place per method rather than at every call site.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{Canvas, Rect};

use super::cursor::CursorOverlay;
use super::drag::DragSession;
use super::held::HeldInput;
use super::inputlink::InputLink;
use super::loginfeed::LoginFeed;
use super::opening::Opening;
use super::origin::OpenHint;
use super::powerfeed::PowerFeed;
use super::prompt::Prompt;
use super::resize::ResizeDrag;
use super::shell::{AltTab, Modifiers, ShellSub};
use super::surface::{Drag, Surface};
use super::themefeed::ThemeFeed;

/// The session compositor's whole mutable state.
pub(super) struct Compositor {
    /// The mapped screen buffer every frame is composed into.
    pub(super) screen: Canvas,
    /// Every live surface; the vector *is* the z-order (tail paints last)
    /// among windows. The desktop paints below and panels above every window
    /// whatever their index; panels keep their creation order.
    pub(super) surfaces: Vec<Surface>,
    /// The pointer position, screen-absolute, as far as input was handled.
    pub(super) pointer: (i32, i32),
    /// The cursor sprite over the composed scene, and the scene under it.
    pub(super) cursor: CursorOverlay,
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
    /// The window whose open zoom is in flight (`opening.rs`).
    pub(super) opening: Option<Opening>,
    /// The placeholder spinner's last painted phase (`spinner.rs`).
    pub(super) spinner_phase: u32,
    /// Whether a pointer button is held (`DragStart` requires it).
    pub(super) button_down: bool,
    /// Buttons whose press the compositor consumed (window buttons, title
    /// bar, bare background): their release is swallowed too, so no surface
    /// sees an unmatched `POINTER_UP` even if focus moved in between.
    pub(super) consumed: u32,
    /// The desktop or panel a press grabbed the pointer to, and the buttons
    /// held on it: it gets every pointer event until they are all released.
    pub(super) grab: Option<(u64, u32)>,
    /// The desktop or panel the pointer hovers, which got the last move.
    pub(super) hover: Option<u64>,
    /// The next surface id to hand out.
    pub(super) next_id: u64,
    /// The `shell` subscriber (issues #167, #157).
    pub(super) shell: Option<ShellSub>,
    /// A privileged non-shell subscriber (issue #447): it sees the shell
    /// events but can never replace the shell.
    pub(super) observer: Option<ShellSub>,
    /// The session that owns the display: the first non-zero session whose
    /// task was accepted as the shell. Only it may claim the shell again
    /// without privilege (a restarted LazyShell), until `logind` reports it
    /// ended (`loginfeed`, issue #623).
    pub(super) display_session: Option<u64>,
    /// The rectangle windows may occupy, as the shell set it; `None` is the
    /// whole screen.
    pub(super) work: Option<Rect>,
    /// The held modifier keys.
    pub(super) mods: Modifiers,
    /// The open Alt+Tab overlay.
    pub(super) alt_tab: Option<AltTab>,
    /// One event-encode buffer for the whole life of the compositor: the user
    /// bump allocator never reclaims, so events reuse it instead of
    /// allocating per message.
    pub(super) scratch: Vec<u8>,
    /// The live `sys/ui/*` theme follower.
    pub(super) themefeed: ThemeFeed,
    /// `init`'s shutdown progress (the shutting-down overlay).
    pub(super) powerfeed: PowerFeed,
    /// `logind`'s logouts (who owns the display next).
    pub(super) loginfeed: LoginFeed,
    /// The compositor's side of `inputd` (`docs/input-plan.md`).
    pub(super) input: InputLink,
    /// Input read during an animation, waiting for the main loop.
    pub(super) held: HeldInput,
    /// Pending open-origin hints, at most one per task.
    pub(super) hints: Vec<OpenHint>,
    /// The shell's `HintLaunchOrigin`, for the next window any task opens.
    pub(super) launch_hint: Option<OpenHint>,
    /// PIT tick at which the next client liveness probe is due.
    pub(super) next_probe: u64,
    /// The trusted prompt `elevd` opened, while it is up (`prompt.rs`).
    pub(super) prompt: Option<Prompt>,
    /// The answered prompt's reply, for the main loop to send.
    pub(super) prompt_reply: Option<(u64, Parcel)>,
    /// The prompt's own `inputd` session, while the prompt is up and
    /// `inputd` is reachable (`prompt_keys.rs`): layout-aware keys from
    /// every keyboard. `None`: the kernel's key stream serves the prompt.
    pub(super) prompt_keys: Option<user::messenger::input::KeySession>,
}

impl Compositor {
    /// A compositor over `screen` with no surfaces and the pointer centered.
    pub(super) fn new(screen: Canvas) -> Compositor {
        let pointer = (screen.width() / 2, screen.height() / 2);
        let mut themefeed = ThemeFeed::new();
        themefeed.decide_scale(screen.width() as u32, screen.height() as u32);
        Compositor {
            screen,
            surfaces: Vec::new(),
            pointer,
            cursor: CursorOverlay::new(),
            focused: None,
            drag: None,
            resize: None,
            last_title_click: None,
            drag_session: None,
            opening: None,
            spinner_phase: 0,
            button_down: false,
            consumed: 0,
            grab: None,
            hover: None,
            next_id: 1,
            shell: None,
            observer: None,
            display_session: None,
            work: None,
            mods: Modifiers::default(),
            alt_tab: None,
            scratch: Vec::with_capacity(64),
            themefeed,
            powerfeed: PowerFeed::new(),
            loginfeed: LoginFeed::new(),
            input: InputLink::new(),
            held: HeldInput::new(),
            hints: Vec::new(),
            launch_hint: None,
            next_probe: 0,
            prompt: None,
            prompt_reply: None,
            prompt_keys: None,
        }
    }

    /// The whole screen as a rectangle.
    pub(super) fn full(&self) -> Rect {
        Rect::new(0, 0, self.screen.width(), self.screen.height())
    }

    /// The rectangle windows may occupy: what the shell set with
    /// `SetWorkArea`, else the whole screen. The single source of the rule
    /// that placement, window movement, resizing and maximize all share.
    pub(super) fn work_area(&self) -> Rect {
        self.work.unwrap_or(self.full())
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
        // A logout ends the display owner's session: the next login's shell
        // may claim the role (issue #623).
        for session in self.loginfeed.poll() {
            if self.display_session == Some(session) {
                user::sys::write_str(&alloc::format!("XUID:LOGOUT session={session}\n"));
                self.display_session = None;
            }
        }
    }

    /// Repaint the full screen.
    pub(super) fn repaint_full(&mut self) {
        let full = self.full();
        self.repaint(full);
    }
}

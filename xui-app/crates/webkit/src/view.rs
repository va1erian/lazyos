//! The page view: one custom-painted node showing the newest frame WebKit
//! painted, forwarding the mouse, wheel and keyboard to the engine.
//!
//! The frame is the engine's bitmap at the size the engine last laid the page
//! out to; while a resize is in flight it is drawn at its own size from the
//! top-left corner over white, never stretched.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Cursor, Event, NodeKind, NodeSpec, Result, TimerId, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Image, Modifiers, MouseButton};

use crate::autoscroll::AutoScroll;
use crate::bands::{Band, Bands};
use crate::engine::Engine;
use crate::events::CursorKind;
use crate::glide::Glide;
use crate::mailbox::{Frame, Update};
use crate::parts::Parts;
use crate::picture;
use crate::smooth::Smooth;
use crate::speculate::{self, Meter, Speculation};
use crate::view_input::{arm_auto, arm_glide, arm_settle, handle};

pub use crate::hooks::{Hooks, IsShortcut, OpenLink};

#[derive(Default)]
pub(crate) struct State {
    /// The picture on show: the last whole frame and what was painted over it since.
    pub(crate) picture: Parts,
    /// Rows of the page outside the view the page's process painted ahead (`bands.rs`).
    pub(crate) bands: RefCell<Bands<Rc<Image>>>,
    /// Microseconds spent drawing the picture, and how many draws, since the window last asked.
    pub(crate) drawn: Cell<(u64, u32)>,
    /// The size last told to the engine.
    pub(crate) size: Cell<(i32, i32)>,
    /// The button held since a press in the view, for moves and captures.
    pub(crate) held: Cell<Option<MouseButton>>,
    /// Modifiers of the newest event that carried them.
    pub(crate) mods: Cell<u32>,
    /// The button of a press kept from the page (`Hooks::swallow_right`, `Hooks::open_link`): its
    /// release is kept too.
    pub(crate) swallowed: Cell<Option<MouseButton>>,
    /// How far the picture is shown ahead of the page (`speculate.rs`).
    pub(crate) spec: RefCell<Speculation>,
    pub(crate) meter: RefCell<Meter>,
    /// The page row shown at the view's top, sliding to where the wheel asked (`glide.rs`), and the
    /// frame timer that moves it; the timer runs only while it slides.
    pub(crate) glide: RefCell<Glide>,
    pub(crate) glide_timer: Cell<Option<TimerId>>,
    /// Frames, gaps and checkerboard while scrolling (`smooth.rs`).
    pub(crate) smooth: RefCell<Smooth>,
    /// When the wheel event that moved the picture ahead arrived, until it is drawn.
    pub(crate) wheel_at: Cell<Option<Instant>>,
    /// With nothing shown ahead: when the last plain vertical notch arrived (`speculate::now_ms`)
    /// and the page pixels the wheel has asked for since the page last answered (positive: down). A
    /// page move soon after, the same way and no larger, is that answer and is glided to; a click or
    /// a key forgets it.
    pub(crate) wheel_asked: Cell<Option<(u64, i32)>>,
    /// The number of the last wheel turn given to the engine: the page answers by it
    /// (`Speculation::answered`).
    pub(crate) wheel_seq: Cell<u64>,
    /// The timer that gives up on a picture shown ahead; it runs only while one is.
    pub(crate) settle: Cell<Option<TimerId>>,
    /// The autoscroll a middle click started (`autoscroll.rs`), and its frame timer, which runs only
    /// while it does.
    pub(crate) auto: RefCell<AutoScroll>,
    pub(crate) auto_timer: Cell<Option<TimerId>>,
    /// The next middle release belongs to a click the autoscroll took: the page never sees it.
    pub(crate) auto_up: Cell<bool>,
    /// The pointer shape the page last asked for: a middle click on a link is the page's.
    pub(crate) cursor: Cell<CursorKind>,
}

impl State {
    /// How far ahead of the page the picture is shown, and the page's offset its pixels were painted at.
    pub(crate) fn shown(&self) -> (i32, i32) {
        let offset = self.spec.borrow().actual().unwrap_or(0);
        let top = self.glide.borrow().top(self.target());
        (top.saturating_sub(offset), offset)
    }

    /// The page row the wheel asked for at the view's top: the page's offset and what is ahead of it.
    pub(crate) fn target(&self) -> i32 {
        let spec = self.spec.borrow();
        spec.actual().unwrap_or(0).saturating_add(spec.shift())
    }

    /// A draw of `rows` rows (`checker` of them checkerboard) that started at `started`, for the counters.
    pub(crate) fn record_draw(&self, started: Instant, rows: u32, checker: u32, ahead: i32) {
        self.smooth
            .borrow_mut()
            .drawn(speculate::now_us(), rows, checker, ahead != 0);
        if let Some(at) = self.wheel_at.take() {
            if ahead != 0 {
                let us = at.elapsed().as_micros() as u64;
                self.meter.borrow_mut().drawn(us);
            }
        }
        let (us, draws) = self.drawn.get();
        self.drawn
            .set((us + started.elapsed().as_micros() as u64, draws + 1));
    }
}

pub struct PageView<M: 'static> {
    control: Rc<Control<M>>,
    state: Rc<RefCell<State>>,
    engine: Rc<dyn Engine>,
}

impl<M: Clone + 'static> PageView<M> {
    pub fn new(ui: &Ui<M>, engine: Rc<dyn Engine>, hooks: Hooks<M>) -> Result<PageView<M>> {
        let control = Rc::new(Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?);
        let state: Rc<RefCell<State>> = Rc::default();
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(state) = state.try_borrow() {
                    let started = Instant::now();
                    let (ahead, offset) = state.shown();
                    let checker = picture::paint(
                        canvas,
                        &state.picture,
                        &state.bands.borrow(),
                        ahead,
                        offset,
                    );
                    let rows = canvas.bounds().height().max(0) as u32;
                    state.record_draw(started, rows, checker, ahead);
                }
            }));
        }
        #[cfg(all(target_os = "linux", target_env = "musl"))]
        crate::view_direct::install(control.id(), &state);
        {
            let (ui, id) = (ui.clone(), control.id());
            let (state, engine) = (Rc::clone(&state), Rc::clone(&engine));
            let arm: Rc<dyn Fn()> = Rc::new({
                let (weak, state) = (Rc::downgrade(&control), Rc::clone(&state));
                move || {
                    arm_settle(&weak, &state);
                    arm_glide(&weak, &state);
                }
            });
            let begin_auto = {
                let (ui, weak) = (ui.clone(), Rc::downgrade(&control));
                let (state, engine, arm) = (Rc::clone(&state), Rc::clone(&engine), Rc::clone(&arm));
                move || arm_auto(&ui, &weak, &state, &engine, &arm)
            };
            let Hooks {
                is_shortcut,
                clicked,
                right_click,
                swallow_right,
                open_link,
            } = hooks;
            control.on_events(move |event| {
                let taken: Cell<Option<M>> = Cell::new(None);
                let swallow = |button: MouseButton, mods: Modifiers| {
                    if button == MouseButton::Right {
                        return swallow_right.as_ref().is_some_and(|ask| ask());
                    }
                    let message = open_link.as_ref().and_then(|ask| ask(button, mods));
                    let kept = message.is_some();
                    taken.set(message);
                    kept
                };
                handle(
                    &ui,
                    id,
                    &state,
                    engine.as_ref(),
                    &is_shortcut,
                    arm.as_ref(),
                    &begin_auto,
                    &swallow,
                    event,
                );
                if let Some(message) = taken.take() {
                    return Some(message);
                }
                match (event, &right_click) {
                    (
                        Event::MouseDown {
                            button: MouseButton::Right,
                            x,
                            y,
                            ..
                        },
                        Some(make),
                    ) => Some(make(*x, *y)),
                    (Event::MouseDown { .. }, _) => Some(clicked.clone()),
                    _ => None,
                }
            });
        }
        Ok(PageView {
            control,
            state,
            engine,
        })
    }

    /// Shows a new whole frame; the previous picture's buffer, to reuse for the next
    /// one, is returned.
    pub fn show(&self, frame: Frame) -> Vec<u8> {
        self.apply(Update {
            full: Some(frame),
            ..Update::default()
        })
        .spare
    }

    /// Shows what the engine painted: a whole new frame, if there is one, then each delta on top
    /// of the picture. [`Applied`] gives back the buffer the old picture left and whether the
    /// picture can no longer follow the engine (a delta it could not apply, too many parts): the
    /// window then asks the mailbox for a whole frame (`Mailbox::request_full`).
    pub fn apply(&self, update: Update) -> Applied {
        let mut state = self.state.borrow_mut();
        let mut spare = Vec::new();
        let mut resync = false;
        let mut changed = false;
        let scroll_y = update.scroll_y;
        if let Some(frame) = update.full {
            match Image::from_rgba(frame.width, frame.height, frame.rgba) {
                Ok(image) => spare = state.picture.set_full(image),
                Err(_) => {
                    state.picture.clear();
                    state.spec.borrow_mut().reset();
                    state.glide.borrow_mut().reset();
                    resync = true;
                }
            }
            changed = true;
        }
        for delta in update.deltas {
            if !state.picture.apply(delta) {
                resync = true;
                break;
            }
            changed = true;
        }
        resync |= state.picture.is_fragmented();
        // The page answered the wheel: the picture glides to where the page went and the turns still
        // unanswered (ahead of the frames that show it, or back when the page did not scroll). Before the
        // frames' offset: the page answered before it painted them, so this offset is the older one.
        let mut answered = false;
        if let (Some((seq, y)), true) = (update.wheel_done, speculate::ahead()) {
            answered = state
                .spec
                .borrow_mut()
                .answered(seq, y, speculate::now_ms());
            changed |= answered;
        }
        let mut glide_back = false;
        let mut glide_answer = false;
        if let Some(y) = scroll_y {
            let before = state.target();
            let known = state.spec.borrow().actual().is_some();
            changed |= state.spec.borrow_mut().report(y, speculate::now_ms());
            // What the page did that the wheel did not ask for (a script, an anchor, going further) is
            // shown at once; catching up with the wheel leaves the target, and the glide, where they are.
            // With nothing shown ahead, the page's answer to a recent notch is glided to instead.
            let after = state.target();
            let answered = !speculate::ahead() && known && {
                let (answer, left) = speculate::answer(
                    state.wheel_asked.get(),
                    after.saturating_sub(before),
                    speculate::now_ms(),
                );
                state.wheel_asked.set(left);
                answer
            };
            if answered {
                state.glide.borrow_mut().start(before, speculate::now_us());
                glide_answer = true;
                changed = true;
            } else {
                state
                    .glide
                    .borrow_mut()
                    .follow(after.saturating_sub(before));
            }
            self.update_reach(&state);
            // Past the end of the page: the picture glides back to it.
            glide_back = state.spec.borrow_mut().set_at_bottom(update.at_bottom);
            changed |= glide_back;
        }
        drop(state);
        if changed {
            self.control.invalidate();
        }
        let weak = Rc::downgrade(&self.control);
        if glide_back || glide_answer || answered {
            arm_glide(&weak, &self.state);
        }
        if answered {
            arm_settle(&weak, &self.state);
        }
        Applied { spare, resync }
    }

    /// Keeps the bands the page's process painted outside the view (`bands.rs`), and lets the
    /// picture be shown ahead as far as they reach.
    pub fn add_bands(&self, bands: Vec<Band>) {
        if bands.is_empty() {
            return;
        }
        let state = self.state.borrow();
        let mut kept = state.bands.borrow_mut();
        for band in bands {
            let (x, y, width, height) = (band.doc_x, band.doc_y, band.width, band.height as i32);
            if let Ok(image) = Image::from_rgba(band.width, band.height, band.rgba) {
                kept.insert(x, y, width, height, Rc::new(image));
            }
        }
        let view_height = state.size.get().1;
        if let Some(top) = state.spec.borrow().actual() {
            kept.prune(top, view_height);
        }
        drop(kept);
        self.update_reach(&state);
    }

    /// A navigation committed: the wait for the new page's first frame is a load (`INFO:TIME`), not a
    /// scroll that stalled.
    pub fn page_committed(&self) {
        let state = self.state.borrow();
        state.smooth.borrow_mut().new_page();
        // What was shown ahead, the turns not answered and the row the picture glides from were the old
        // document's: kept, the new page would be drawn at the old one's row (a checkerboard) until it moved.
        state.spec.borrow_mut().reset();
        state
            .spec
            .borrow_mut()
            .forget_turns_through(state.wheel_seq.get());
        state.glide.borrow_mut().reset();
    }

    /// The page on show is another one: what was painted outside the view belongs to the old one.
    pub fn forget_bands(&self) {
        // Those still queued are the old page's too.
        drop(crate::bands::take());
        let state = self.state.borrow();
        state.bands.borrow_mut().clear();
        self.update_reach(&state);
    }

    /// How far ahead the picture may be shown: as far as the bands reach past the view's edges.
    fn update_reach(&self, state: &State) {
        let mut spec = state.spec.borrow_mut();
        let Some(top) = spec.actual() else {
            return;
        };
        let height = state.size.get().1;
        let bands = state.bands.borrow();
        let down = bands.reach(top.saturating_add(height), true);
        let up = bands.reach(top, false);
        spec.set_reach(down, up);
    }

    /// Shows the blank page again (the page's process is gone).
    pub fn clear(&self) {
        let had = !self.state.borrow().picture.is_empty();
        self.state.borrow_mut().picture.clear();
        self.state.borrow().bands.borrow_mut().clear();
        self.state.borrow().spec.borrow_mut().reset();
        self.state.borrow().glide.borrow_mut().reset();
        if had {
            self.control.invalidate();
        }
    }

    /// The size of the picture on show.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        let state = self.state.borrow();
        (!state.picture.is_empty()).then(|| state.picture.size())
    }

    /// How many images the picture on show is made of (for the window's counters).
    pub fn picture_parts(&self) -> usize {
        self.state.borrow().picture.len()
    }

    /// The `INFO:SPEC:` line for the last second, when the picture was moved ahead of the page in it.
    pub fn spec_line(&self) -> Option<String> {
        let state = self.state.borrow();
        let (counters, ahead) = {
            let spec = state.spec.borrow();
            (spec.counters(), spec.shift())
        };
        let line = state
            .meter
            .borrow_mut()
            .line(speculate::now_ms(), counters, ahead);
        line
    }

    /// The `INFO:SMOOTH:` line for the last second, when the wheel turned in it.
    pub fn smooth_line(&self) -> Option<String> {
        let state = self.state.borrow();
        let line = state.smooth.borrow_mut().line(speculate::now_ms());
        line
    }

    /// The time spent drawing the picture (microseconds) and the draws, since the last call.
    pub fn take_draw_time(&self) -> (u64, u32) {
        self.state.borrow().drawn.take()
    }

    pub fn focus(&self) {
        self.control.focus();
    }

    pub fn engine(&self) -> &Rc<dyn Engine> {
        &self.engine
    }

    /// The pointer shape over the page, as the page asked for it.
    pub fn set_cursor(&self, kind: CursorKind) {
        let state = self.state.borrow();
        state.cursor.set(kind);
        if state.auto.borrow().is_active() {
            // The autoscroll's pointer stays until it ends.
            return;
        }
        drop(state);
        self.control
            .ui()
            .set_cursor(self.control.id(), cursor(kind));
    }
}

/// The xui pointer for a page's shape: the compositor has an arrow, a hand, an I-beam and the
/// resize arrows (busy is a window-wide state, and a hidden pointer is not offered).
pub(crate) fn cursor(kind: CursorKind) -> Cursor {
    match kind {
        CursorKind::Hand => Cursor::Hand,
        CursorKind::Text => Cursor::Text,
        CursorKind::ResizeHorizontal => Cursor::SizeHorizontal,
        CursorKind::ResizeVertical => Cursor::SizeVertical,
        CursorKind::ResizeNwSe => Cursor::SizeNwSe,
        CursorKind::ResizeNeSw => Cursor::SizeNeSw,
        CursorKind::Default | CursorKind::Wait | CursorKind::Hidden => Cursor::Default,
    }
}

impl<M: Clone + 'static> Placeable<M> for PageView<M> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<M>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// The page is laid out at the size the view is given.
    fn placed(&self, _ui: &Ui<M>, rect: Rect) {
        let size = (rect.width().max(1), rect.height().max(1));
        let state = self.state.borrow();
        if state.size.replace(size) != size {
            state.spec.borrow_mut().reset();
            state.glide.borrow_mut().reset();
            state.bands.borrow_mut().clear();
            self.engine.resize(size.0, size.1);
        }
    }
}

/// What [`PageView::apply`] leaves for the window to do.
#[derive(Debug, Default)]
pub struct Applied {
    /// The previous whole picture's buffer, to be filled again by the next frame.
    pub spare: Vec<u8>,
    /// The picture cannot follow the engine any more: ask for a whole frame.
    pub resync: bool,
}

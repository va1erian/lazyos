//! The picture view: one custom-painted node that shows the newest decoded
//! frame, letterboxed on black, or a note when there is none. A timer pulls
//! frames from the player thread while something is moving.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyvideo::Player;
use xui_core::app::Ui;
use xui_core::backend::{
    Canvas, NodeKind, NodeSpec, Result, TextAlign, TextStyle, TextVAlign, TimerId, WidgetId,
};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Dip, Image};

use crate::app::Msg;

/// The frame poll: fast enough for 60 frames a second.
const TICK_MS: u32 = 16;

#[derive(Default)]
struct State {
    image: Option<Image>,
    /// Shown instead of a picture: the hint, or why playback failed.
    note: String,
    failed: bool,
}

pub struct VideoView {
    control: Control<Msg>,
    state: Rc<RefCell<State>>,
    timer: Cell<Option<TimerId>>,
}

impl VideoView {
    pub fn new(ui: &Ui<Msg>) -> Result<VideoView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state: Rc<RefCell<State>> = Rc::default();
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(state) = state.try_borrow() {
                    paint(canvas, &state);
                }
            }));
        }
        Ok(VideoView {
            control,
            state,
            timer: Cell::new(None),
        })
    }

    /// Shows `note` (and no picture) until the next frame.
    pub fn set_note(&self, note: &str, failed: bool) {
        let mut state = self.state.borrow_mut();
        state.image = None;
        state.note = note.to_owned();
        state.failed = failed;
        drop(state);
        self.control.invalidate();
    }

    /// Takes the player's newest frame, if it has one, and repaints.
    pub fn pump(&self, player: &Player) -> bool {
        let Some(frame) = player.take_frame() else {
            return false;
        };
        let mut state = self.state.borrow_mut();
        // The previous picture's pixels go back to the decoder.
        if let Some(old) = state.image.take() {
            player.recycle(old.into_pixels());
        }
        state.image = Image::from_rgba(frame.width, frame.height, frame.rgba).ok();
        state.note.clear();
        state.failed = false;
        drop(state);
        self.control.invalidate();
        true
    }

    /// The size of the picture showing.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.state
            .borrow()
            .image
            .as_ref()
            .map(|image| (image.width(), image.height()))
    }

    /// Starts or stops the frame timer.
    pub fn run_timer(&self, on: bool) {
        match (on, self.timer.get()) {
            (true, None) => self
                .timer
                .set(self.control.set_timer(TICK_MS, || Some(Msg::Tick))),
            (false, Some(timer)) => {
                self.timer.set(None);
                self.control.kill_timer(timer);
            }
            _ => {}
        }
    }

    pub fn timer_running(&self) -> bool {
        self.timer.get().is_some()
    }

    pub fn focus(&self, ui: &Ui<Msg>) {
        ui.focus(self.control.id());
    }
}

impl Placeable<Msg> for VideoView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// `picture` (w, h) fitted inside `bounds`, centred, aspect kept.
pub(crate) fn letterbox(bounds: Rect, w: u32, h: u32) -> Rect {
    let (bw, bh) = (bounds.width().max(0) as i64, bounds.height().max(0) as i64);
    if w == 0 || h == 0 || bw == 0 || bh == 0 {
        return Rect::new(bounds.left, bounds.top, bounds.left, bounds.top);
    }
    // Compare w/h with bw/bh without division.
    let (fw, fh) = if (w as i64) * bh >= (h as i64) * bw {
        (bw, (h as i64 * bw / w as i64).max(1))
    } else {
        ((w as i64 * bh / h as i64).max(1), bh)
    };
    let left = bounds.left + ((bw - fw) / 2) as i32;
    let top = bounds.top + ((bh - fh) / 2) as i32;
    Rect::new(left, top, left + fw as i32, top + fh as i32)
}

fn paint(canvas: &mut dyn Canvas, state: &State) {
    let bounds = canvas.bounds();
    canvas.push_clip(bounds);
    canvas.fill_rect(bounds, Color::rgb(0, 0, 0));
    match &state.image {
        Some(image) => {
            canvas.draw_image(image, letterbox(bounds, image.width(), image.height()));
        }
        None if !state.note.is_empty() => {
            let ink = if state.failed {
                Color::rgb(0xff, 0x8a, 0x80)
            } else {
                Color::rgb(0xd0, 0xd0, 0xd0)
            };
            let mut style = TextStyle::new(ink, Dip(14.0));
            style.align = TextAlign::Center;
            style.valign = TextVAlign::Middle;
            style.wrap = true;
            let inset = Rect::new(
                bounds.left + 24,
                bounds.top,
                bounds.right - 24,
                bounds.bottom,
            );
            canvas.draw_text(&state.note, inset, &style);
        }
        None => {}
    }
    canvas.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(w: i32, h: i32) -> Rect {
        Rect::new(10, 20, 10 + w, 20 + h)
    }

    #[test]
    fn a_wide_picture_in_a_tall_box_leaves_bars_above_and_below() {
        let out = letterbox(r(400, 400), 160, 90);
        assert_eq!((out.width(), out.height()), (400, 225));
        assert_eq!(out.left, 10);
        assert_eq!(out.top, 20 + (400 - 225) / 2);
    }

    #[test]
    fn a_tall_picture_in_a_wide_box_leaves_bars_at_the_sides() {
        let out = letterbox(r(400, 200), 100, 200);
        assert_eq!((out.width(), out.height()), (100, 200));
        assert_eq!(out.left, 10 + 150);
        assert_eq!(out.top, 20);
    }

    #[test]
    fn a_matching_aspect_fills_the_box() {
        let out = letterbox(r(640, 360), 1280, 720);
        assert_eq!(
            (out.left, out.top, out.width(), out.height()),
            (10, 20, 640, 360)
        );
    }

    #[test]
    fn an_empty_box_or_picture_gives_an_empty_rect() {
        assert_eq!(letterbox(r(0, 50), 10, 10).width(), 0);
        assert_eq!(letterbox(r(50, 50), 0, 10).width(), 0);
    }
}

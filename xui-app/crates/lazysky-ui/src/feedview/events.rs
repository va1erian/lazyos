//! FeedView input (§6.3): wheel, keys, the scroll bar, middle-button
//! autoscroll, and hit testing against the rows' prepared regions.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, WidgetId};
use xui_core::message::{Key, MouseButton};

use super::paint::thumb;
use super::{FeedAction, Grab, ViewState, BAR_96, WHEEL_96};

/// The `MouseWheel` delta of one notch.
const WHEEL_DELTA: i32 = 120;
use crate::Msg;

pub(super) fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<ViewState>>,
    pane: u8,
    event: &Event,
) -> Option<Msg> {
    let outcome = {
        let mut s = state.borrow_mut();
        route(ui, id, &mut s, event)
    };
    match outcome {
        Outcome::Nothing => None,
        Outcome::Moved => {
            state.borrow_mut().moved = true;
            ui.invalidate(id);
            None
        }
        Outcome::Repaint => {
            ui.invalidate(id);
            None
        }
        Outcome::Action(action) => Some(Msg::Feed(pane, action)),
        Outcome::Back => Some(Msg::Back),
    }
}

enum Outcome {
    Nothing,
    /// The position changed.
    Moved,
    /// Only the picture changed.
    Repaint,
    Action(FeedAction),
    /// Alt+Left: back out of this page.
    Back,
}

fn scrolled(s: &mut ViewState, dy: i32) -> Outcome {
    let before = s.feed.scroll_top();
    s.feed.scroll_by(dy);
    if s.feed.scroll_top() == before {
        Outcome::Nothing
    } else {
        Outcome::Moved
    }
}

fn route(ui: &Ui<Msg>, id: WidgetId, s: &mut ViewState, event: &Event) -> Outcome {
    match *event {
        Event::MouseWheel {
            delta, horizontal, ..
        } if !horizontal => {
            // `delta` is in 120ths of a notch (Windows' WHEEL_DELTA, which the
            // LazyOS backend also reports): one notch is `WHEEL_96` pixels.
            let step = -i32::from(delta) * s.scaled(WHEEL_96) / WHEEL_DELTA;
            scrolled(s, step)
        }
        Event::KeyDown {
            key: Key::LEFT,
            modifiers,
            ..
        } if modifiers.alt => Outcome::Back,
        Event::KeyDown { key, modifiers, .. } if !modifiers.ctrl && !modifiers.alt => {
            let page = (s.height * 9 / 10).max(1);
            let line = s.scaled(WHEEL_96);
            match key {
                Key::PAGE_DOWN => scrolled(s, page),
                Key::SPACE if modifiers.shift => scrolled(s, -page),
                Key::SPACE => scrolled(s, page),
                Key::PAGE_UP => scrolled(s, -page),
                Key::DOWN => scrolled(s, line),
                Key::UP => scrolled(s, -line),
                Key::HOME => scrolled(s, -s.feed.scroll_top()),
                Key::END => scrolled(s, s.feed.max_scroll()),
                Key::ESCAPE if s.autoscroll.is_some() => {
                    s.autoscroll = None;
                    Outcome::Repaint
                }
                _ => Outcome::Nothing,
            }
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            ui.focus(id);
            if s.autoscroll.take().is_some() {
                return Outcome::Repaint;
            }
            let bar = s.scaled(BAR_96);
            if x >= s.width - bar {
                return bar_press(ui, id, s, y);
            }
            click(s, x, y)
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Right,
            ..
        } => {
            ui.focus(id);
            match click(s, x, y) {
                Outcome::Action(FeedAction::Target { row, target, at }) => {
                    Outcome::Action(FeedAction::Context { row, target, at })
                }
                _ => Outcome::Nothing,
            }
        }
        Event::MouseDown {
            y,
            button: MouseButton::Middle,
            ..
        } => {
            ui.focus(id);
            s.autoscroll = match s.autoscroll {
                Some(_) => None,
                None => Some((y, y)),
            };
            Outcome::Repaint
        }
        Event::MouseMove { y, .. } => {
            if let Some(grab) = s.grab {
                let (total, view) = (s.feed.total_height(), s.height.max(1));
                let ratio = total as f32 / view as f32;
                let to = grab.scroll + ((y - grab.from) as f32 * ratio) as i32;
                let before = s.feed.scroll_top();
                s.feed.set_scroll_top(to);
                return if s.feed.scroll_top() == before {
                    Outcome::Nothing
                } else {
                    Outcome::Moved
                };
            }
            if let Some((origin, _)) = s.autoscroll {
                s.autoscroll = Some((origin, y));
            }
            Outcome::Nothing
        }
        Event::MouseUp {
            button: MouseButton::Left,
            ..
        } => {
            if s.grab.take().is_some() {
                ui.release_capture();
            }
            Outcome::Nothing
        }
        Event::CaptureChanged | Event::MouseLeave => {
            s.grab = None;
            Outcome::Nothing
        }
        _ => Outcome::Nothing,
    }
}

/// A press on the scroll bar: grab the thumb, or page towards the press.
fn bar_press(ui: &Ui<Msg>, id: WidgetId, s: &mut ViewState, y: i32) -> Outcome {
    let Some((at, len)) = thumb(s) else {
        return Outcome::Nothing;
    };
    if y >= at && y < at + len {
        s.grab = Some(Grab {
            from: y,
            scroll: s.feed.scroll_top(),
        });
        ui.set_capture(id);
        return Outcome::Nothing;
    }
    let page = (s.height * 9 / 10).max(1);
    scrolled(s, if y < at { -page } else { page })
}

/// A click at widget-local `(x, y)`: a hit region of the row under it, or
/// the row.
fn click(s: &ViewState, x: i32, y: i32) -> Outcome {
    let feed = &s.feed;
    if feed.is_empty() {
        return Outcome::Nothing;
    }
    let content_y = feed.scroll_top() + y;
    let Some(i) = feed.row_at(content_y) else {
        return Outcome::Nothing;
    };
    let Some(row) = feed.row(i) else {
        return Outcome::Nothing;
    };
    if content_y >= feed.row_top(i) + row.height {
        return Outcome::Nothing;
    }
    let (lx, ly) = (x, content_y - feed.row_top(i));
    let hit = row.prepared.as_ref().and_then(|p| {
        p.hits.iter().find(|h| {
            lx >= h.rect.left && lx < h.rect.right && ly >= h.rect.top && ly < h.rect.bottom
        })
    });
    match hit {
        Some(hit) => Outcome::Action(FeedAction::Target {
            row: row.id,
            target: hit.target.clone(),
            at: (x, y),
        }),
        None => Outcome::Action(FeedAction::Row(row.id)),
    }
}

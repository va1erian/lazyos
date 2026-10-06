//! The page view: a custom-painted node that draws the [`Viewer`]'s pages
//! (preview first, then tiles over it), its own scroll bars, and maps the
//! wheel and the bars to scrolling. It owns the timer that drains the render
//! threads while they work.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, TextStyle, TimerId, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Dip, Theme};

use crate::app::Msg;
use crate::cache::Key;
use crate::layout::PxRect;
use crate::viewer::Viewer;

/// How often the timer drains finished tiles while the threads work.
const TICK_MS: u32 = 30;
/// One wheel notch, in pixels at 96 dpi.
const WHEEL_96: i32 = 48;
/// The scroll bars' thickness at 96 dpi.
const BAR_96: i32 = 10;

/// Which bar a drag holds, and where it grabbed it.
#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Vertical { from: i32, offset: i32 },
    Horizontal { from: i32, offset: i32 },
}

pub struct PageView {
    control: Control<Msg>,
    pub viewer: Rc<RefCell<Viewer>>,
    timer: Cell<Option<TimerId>>,
}

impl PageView {
    pub fn new(ui: &Ui<Msg>, viewer: Rc<RefCell<Viewer>>) -> Result<PageView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        {
            let viewer = Rc::clone(&viewer);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint(canvas, &viewer.borrow(), &theme.get())
            }));
        }
        {
            let viewer = Rc::clone(&viewer);
            let grab: Rc<Cell<Option<Grab>>> = Rc::default();
            let ui = ui.clone();
            let id = control.id();
            control.on_events(move |event| handle(&ui, id, &viewer, &grab, event));
        }
        Ok(PageView {
            control,
            viewer,
            timer: Cell::new(None),
        })
    }

    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// After the viewer changed: ask for the tiles it now needs, start
    /// draining, and repaint.
    pub fn refresh(&self) {
        self.viewer.borrow_mut().schedule();
        if self.viewer.borrow().busy() && self.timer.get().is_none() {
            self.timer
                .set(self.control.set_timer(TICK_MS, || Some(Msg::Tick)));
        }
        self.control.invalidate();
    }

    /// A timer tick: take finished tiles; stop the timer once all are in.
    pub fn tick(&self) {
        let arrived = self.viewer.borrow_mut().drain();
        if arrived {
            self.control.invalidate();
        }
        if !self.viewer.borrow().busy() {
            // A tile can land between the drain and the busy check.
            if self.viewer.borrow_mut().drain() {
                self.control.invalidate();
            }
            if let Some(timer) = self.timer.take() {
                self.control.kill_timer(timer);
            }
        }
    }

    /// Whether the timer is draining (tests pump it by hand).
    pub fn draining(&self) -> bool {
        self.timer.get().is_some() || self.viewer.borrow().busy()
    }
}

impl Placeable<Msg> for PageView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        let dpi = self.control.dpi();
        self.viewer
            .borrow_mut()
            .set_view(rect.width(), rect.height(), dpi);
        self.refresh();
    }
}

fn scaled(px_96: i32, dpi: u32) -> i32 {
    (px_96 * dpi.max(1) as i32 + 48) / 96
}

/// The scroll bars' thumbs in widget-local pixels: (vertical, horizontal).
fn thumbs(v: &Viewer) -> (Option<PxRect>, Option<PxRect>) {
    let bar = scaled(BAR_96, v.dpi);
    let (w, h) = v.view;
    let thumb = |view: i32, total: i32, offset: i32, track: i32| {
        let len = ((view as i64 * track as i64) / total.max(1) as i64).max(bar as i64 * 2) as i32;
        let len = len.min(track);
        let room = (total - view).max(1);
        let at = ((offset as i64 * (track - len) as i64) / room as i64) as i32;
        (at, len)
    };
    let vertical = (v.layout.height > h).then(|| {
        let (at, len) = thumb(h, v.layout.height, v.offset.1, h);
        PxRect {
            x: w - bar,
            y: at,
            w: bar,
            h: len,
        }
    });
    let horizontal = (v.layout.width > w).then(|| {
        let track = w - if vertical.is_some() { bar } else { 0 };
        let (at, len) = thumb(w, v.layout.width, v.offset.0, track);
        PxRect {
            x: at,
            y: h - bar,
            w: len,
            h: bar,
        }
    });
    (vertical, horizontal)
}

fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    viewer: &Rc<RefCell<Viewer>>,
    grab: &Rc<Cell<Option<Grab>>>,
    event: &Event,
) -> Option<Msg> {
    match *event {
        Event::MouseWheel {
            delta,
            horizontal,
            modifiers,
            ..
        } => {
            if modifiers.ctrl && !horizontal {
                return Some(Msg::ZoomStep(delta > 0));
            }
            let step = -i32::from(delta) * scaled(WHEEL_96, viewer.borrow().dpi);
            let moved = if horizontal || modifiers.shift {
                viewer.borrow_mut().scroll_by(step, 0)
            } else {
                viewer.borrow_mut().scroll_by(0, step)
            };
            moved.then_some(Msg::Scrolled)
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            ui.focus(id);
            let v = viewer.borrow();
            let (vertical, horizontal) = thumbs(&v);
            let bar = scaled(BAR_96, v.dpi);
            if let Some(t) = vertical.filter(|_| x >= v.view.0 - bar) {
                if y >= t.y && y < t.bottom() {
                    grab.set(Some(Grab::Vertical {
                        from: y,
                        offset: v.offset.1,
                    }));
                    ui.set_capture(id);
                    return None;
                }
                // A click on the track pages towards it.
                let page = if y < t.y { -v.view.1 } else { v.view.1 };
                drop(v);
                return viewer
                    .borrow_mut()
                    .scroll_by(0, page * 9 / 10)
                    .then_some(Msg::Scrolled);
            }
            if let Some(t) = horizontal.filter(|_| y >= v.view.1 - bar) {
                if x >= t.x && x < t.right() {
                    grab.set(Some(Grab::Horizontal {
                        from: x,
                        offset: v.offset.0,
                    }));
                    ui.set_capture(id);
                    return None;
                }
                let page = if x < t.x { -v.view.0 } else { v.view.0 };
                drop(v);
                return viewer
                    .borrow_mut()
                    .scroll_by(page * 9 / 10, 0)
                    .then_some(Msg::Scrolled);
            }
            None
        }
        Event::MouseMove { x, y, .. } => {
            let held = grab.get()?;
            let mut v = viewer.borrow_mut();
            let moved = match held {
                Grab::Vertical { from, offset } => {
                    let ratio = v.layout.height as f32 / v.view.1.max(1) as f32;
                    let to = offset + ((y - from) as f32 * ratio) as i32;
                    let x0 = v.offset.0;
                    v.scroll_to(x0, to)
                }
                Grab::Horizontal { from, offset } => {
                    let ratio = v.layout.width as f32 / v.view.0.max(1) as f32;
                    let to = offset + ((x - from) as f32 * ratio) as i32;
                    let y0 = v.offset.1;
                    v.scroll_to(to, y0)
                }
            };
            moved.then_some(Msg::Scrolled)
        }
        Event::MouseUp { .. } => {
            if grab.take().is_some() {
                ui.release_capture();
            }
            None
        }
        Event::CaptureChanged => {
            grab.set(None);
            None
        }
        _ => None,
    }
}

/// The grey the pages sit on.
fn desk(theme: &Theme) -> Color {
    if theme.is_dark {
        Color::rgb(0x26, 0x26, 0x28)
    } else {
        Color::rgb(0x8a, 0x8d, 0x91)
    }
}

fn paint(canvas: &mut dyn Canvas, v: &Viewer, theme: &Theme) {
    let bounds = canvas.bounds();
    canvas.push_clip(bounds);
    canvas.clear(desk(theme));
    let place = |r: PxRect| {
        let left = bounds.left + r.x - v.offset.0;
        let top = bounds.top + r.y - v.offset.1;
        Rect::new(left, top, left + r.w, top + r.h)
    };
    let view = v.view_rect();
    let scale = v.layout.scale;
    for page in v.layout.visible(view.y, view.h) {
        let rect = v.layout.rects[page];
        let on_screen = place(rect);
        canvas.fill_rect(on_screen, Color::rgb(0xff, 0xff, 0xff));
        if let Some(preview) = v.image(&Key::Preview { page: page as u32 }) {
            canvas.draw_image(preview, on_screen);
        }
        for (col, row) in v.layout.tiles(page, view) {
            let (Some(image), Some(t)) = (
                v.image(&Key::tile(page, scale, col, row)),
                v.layout.tile_rect(page, col, row),
            ) else {
                continue;
            };
            canvas.draw_image(
                image,
                place(PxRect {
                    x: rect.x + t.x,
                    y: rect.y + t.y,
                    ..t
                }),
            );
        }
        canvas.stroke_rect(on_screen, Color::rgb(0x40, 0x40, 0x40), 1.0);
    }
    let note = match (&v.error, v.page_count()) {
        (Some(error), _) => Some(error.as_str()),
        (None, 0) => Some("Open a PDF document with Ctrl+O, or drop one here."),
        _ => None,
    };
    if let Some(note) = note {
        let mut style = TextStyle::new(Color::rgb(0xff, 0xff, 0xff), Dip(14.0));
        style.align = xui_core::backend::TextAlign::Center;
        style.valign = xui_core::backend::TextVAlign::Middle;
        style.wrap = true;
        let inset = Rect::new(
            bounds.left + 24,
            bounds.top,
            bounds.right - 24,
            bounds.bottom,
        );
        canvas.draw_text(note, inset, &style);
    }
    let (vertical, horizontal) = thumbs(v);
    for thumb in [vertical, horizontal].into_iter().flatten() {
        let r = Rect::new(
            bounds.left + thumb.x + 2,
            bounds.top + thumb.y + 2,
            bounds.left + thumb.right() - 2,
            bounds.top + thumb.bottom() - 2,
        );
        canvas.fill_rounded_rect(r, 3.0, Color::rgb(0xd0, 0xd0, 0xd0));
    }
    canvas.pop_clip();
}

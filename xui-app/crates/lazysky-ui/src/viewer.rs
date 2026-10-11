//! The image viewer (docs/lazysky-plan.md P7): one picture, fitted to the
//! window, zoomed with Ctrl+= / Ctrl+- / Ctrl+0, panned by dragging,
//! saved with Ctrl+S.
//!
//! The viewer never scales a bitmap itself (R1, R4): a zoom asks the core
//! for the picture at the new size (`Shape::Fit`) and keeps drawing the
//! previous one until it arrives.

use std::cell::RefCell;
use std::rc::Rc;

use lazysky_core::{ImageKey, Shape};
use xui_core::app::Ui;
use xui_core::arrange::{build as create, button, column, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::{Key, MouseButton};
use xui_core::widget::{Control, Label, Placeable};
use xui_core::{HasText, Theme};

use crate::images::ImageCache;
use crate::Msg;

/// Zoom steps: each press multiplies by this.
const STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 8.0;
/// The most pixels the viewer asks for (16 Mpx is 64 MB of RGBA).
const MAX_PIXELS: u64 = 16 * 1000 * 1000;

/// What the viewer shows and how.
pub struct ViewerState {
    /// The picture's full-size URL.
    pub url: Option<String>,
    /// 1.0 is "fit the window".
    pub zoom: f32,
    /// How far the picture is dragged from centre.
    pub offset: (i32, i32),
    /// The view in device pixels.
    pub size: (i32, i32),
    /// The key last asked for (so the same one is not asked twice).
    pub requested: Option<ImageKey>,
    /// When `requested` was asked for (the arrival time is reported).
    pub asked_at: Option<std::time::Instant>,
    /// The latest key whose picture was drawn: kept while a new one loads.
    pub shown: Option<ImageKey>,
    drag: Option<(i32, i32, (i32, i32))>,
}

impl ViewerState {
    fn new() -> ViewerState {
        ViewerState {
            url: None,
            zoom: 1.0,
            offset: (0, 0),
            size: (0, 0),
            requested: None,
            asked_at: None,
            shown: None,
            drag: None,
        }
    }

    /// The picture to want at the current size and zoom.
    pub fn wanted(&self) -> Option<ImageKey> {
        let url = self.url.clone()?;
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return None;
        }
        // Whole-pixel boxes, never past the decoder's side limit and never
        // past a pixel budget: a zoomed `Fit` bitmap is RGBA in the image
        // thread and again in the cache, and zoom steps are cached keys.
        let side = |n: i32| ((n as f32 * self.zoom).round() as u32).clamp(1, 8192);
        let (mut width, mut height) = (side(w), side(h));
        let area = u64::from(width) * u64::from(height);
        if area > MAX_PIXELS {
            let shrink = (MAX_PIXELS as f32 / area as f32).sqrt();
            width = ((width as f32 * shrink) as u32).max(1);
            height = ((height as f32 * shrink) as u32).max(1);
        }
        Some(ImageKey {
            url,
            width,
            height,
            shape: Shape::Fit,
        })
    }

    pub fn open(&mut self, url: String) {
        self.url = Some(url);
        self.zoom = 1.0;
        self.offset = (0, 0);
        self.requested = None;
        self.shown = None;
    }

    pub fn zoom_by(&mut self, steps: i32) {
        let factor = STEP.powi(steps);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if self.zoom <= MIN_ZOOM {
            self.offset = (0, 0);
        }
    }

    pub fn fit(&mut self) {
        self.zoom = 1.0;
        self.offset = (0, 0);
    }
}

pub struct Viewer {
    title: Rc<Label<Msg>>,
    pub state: Rc<RefCell<ViewerState>>,
    control: Rc<ViewerControl>,
    _mounted: Mounted<Msg>,
}

impl Viewer {
    pub fn build(ui: &Ui<Msg>, page: WidgetId, images: &Rc<RefCell<ImageCache>>) -> Result<Viewer> {
        let (title, control) = (Handle::new(), Handle::new());
        let state = Rc::new(RefCell::new(ViewerState::new()));
        let (shared, cache) = (Rc::clone(&state), Rc::clone(images));
        let mounted = ui.mount_in(
            page,
            column().gap(0).children((
                row().padding(8).gap(8).children((
                    button("Back").on_click(Msg::Back),
                    label("").title().bind(&title).fill(1),
                    button("Save as...").on_click(Msg::ViewerSave),
                    button("Zoom out").on_click(Msg::ViewerZoom(-1)),
                    button("Zoom in").on_click(Msg::ViewerZoom(1)),
                    button("Fit").on_click(Msg::ViewerFit),
                )),
                create(move |ui: &Ui<Msg>| {
                    ViewerControl::new(ui, Rc::clone(&shared), Rc::clone(&cache))
                })
                .bind(&control)
                .fill(1),
            )),
        )?;
        Ok(Viewer {
            title: title.get(),
            state,
            control: control.get(),
            _mounted: mounted,
        })
    }

    pub fn set_title(&self, text: &str) {
        self.title.set_text(text);
    }

    pub fn focus(&self) {
        self.control.control.focus();
    }

    pub fn invalidate(&self) {
        self.control.ui.invalidate(self.control.control.id());
    }
}

/// The custom-painted area under the viewer's header.
pub struct ViewerControl {
    control: Control<Msg>,
    ui: Ui<Msg>,
    state: Rc<RefCell<ViewerState>>,
}

impl ViewerControl {
    fn new(
        ui: &Ui<Msg>,
        state: Rc<RefCell<ViewerState>>,
        images: Rc<RefCell<ImageCache>>,
    ) -> Result<ViewerControl> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        {
            let (state, theme) = (Rc::clone(&state), ui.theme_handle());
            control.set_painter(Rc::new(move |canvas| {
                if let (Ok(mut state), Ok(images)) = (state.try_borrow_mut(), images.try_borrow()) {
                    paint(canvas, &mut state, &images, &theme.get());
                }
            }));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.on_events(move |event| handle(&ui, id, &state, event));
        }
        Ok(ViewerControl {
            control,
            ui: ui.clone(),
            state,
        })
    }
}

impl Placeable<Msg> for ViewerControl {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.state.borrow_mut().size = (rect.width(), rect.height());
        self.ui.invalidate(self.control.id());
    }
}

fn paint(canvas: &mut dyn Canvas, state: &mut ViewerState, images: &ImageCache, theme: &Theme) {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);
    // The picture for the wanted key if it is ready, else the last one drawn.
    let ready = state.wanted().filter(|key| images.contains(key));
    if let Some(key) = ready {
        state.shown = Some(key);
    }
    let Some(image) = state.shown.as_ref().and_then(|key| images.get(key)) else {
        let mut style = xui_core::TextStyle::new(theme.text_secondary, xui_core::Dip(14.0));
        style.align = xui_core::backend::TextAlign::Center;
        style.valign = xui_core::backend::TextVAlign::Middle;
        let failed = state.wanted().is_some_and(|key| images.has_failed(&key));
        let text = if failed { "This picture could not be shown" } else { "Loading..." };
        canvas.draw_text(text, bounds, &style);
        return;
    };
    let (iw, ih) = (image.width() as i32, image.height() as i32);
    let (cx, cy) = (
        bounds.left + bounds.width() / 2 + state.offset.0,
        bounds.top + bounds.height() / 2 + state.offset.1,
    );
    let rect = Rect::new(cx - iw / 2, cy - ih / 2, cx - iw / 2 + iw, cy - ih / 2 + ih);
    // One clip for the whole frame: a zoomed picture is larger than the view.
    canvas.push_clip(bounds);
    canvas.draw_image(image, rect);
    canvas.pop_clip();
}

fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<ViewerState>>,
    event: &Event,
) -> Option<Msg> {
    let mut s = state.borrow_mut();
    match *event {
        Event::KeyDown { key, modifiers, .. } => {
            if modifiers.alt && key == Key::LEFT {
                return Some(Msg::Back);
            }
            if modifiers.ctrl {
                // Virtual-key codes: 0xBB is `=`/`+`, 0xBD `-`, 0x6B and
                // 0x6D the numpad's.
                return match (key, key.code()) {
                    (Key::S, _) => Some(Msg::ViewerSave),
                    (Key::DIGIT0, _) => Some(Msg::ViewerFit),
                    (_, 0xBB | 0x6B) => Some(Msg::ViewerZoom(1)),
                    (_, 0xBD | 0x6D) => Some(Msg::ViewerZoom(-1)),
                    _ => None,
                };
            }
            match key {
                Key::ESCAPE => Some(Msg::Back),
                Key::LEFT => Some(Msg::ViewerStep(-1)),
                Key::RIGHT => Some(Msg::ViewerStep(1)),
                _ => None,
            }
        }
        Event::MouseWheel {
            delta, modifiers, ..
        } if modifiers.ctrl => Some(Msg::ViewerZoom(i32::from(delta.signum()))),
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            ui.focus(id);
            if s.zoom > MIN_ZOOM {
                s.drag = Some((x, y, s.offset));
                ui.set_capture(id);
            }
            None
        }
        Event::MouseMove { x, y, .. } => {
            if let Some((fx, fy, from)) = s.drag {
                s.offset = (from.0 + x - fx, from.1 + y - fy);
                drop(s);
                ui.invalidate(id);
            }
            None
        }
        Event::MouseUp { .. } | Event::CaptureChanged => {
            if s.drag.take().is_some() {
                drop(s);
                ui.release_capture();
            }
            None
        }
        _ => None,
    }
}

/// A point of the view's centre, for tests.
pub fn centre(size: (i32, i32)) -> Point {
    Point::new(size.0 / 2, size.1 / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_stay_in_range_and_fit_resets() {
        let mut s = ViewerState::new();
        s.open("https://c.test/p@jpeg".into());
        s.size = (800, 600);
        let fit = s.wanted().unwrap();
        assert_eq!((fit.width, fit.height, fit.shape), (800, 600, Shape::Fit));
        s.zoom_by(2);
        assert!((s.zoom - 1.5625).abs() < 1e-4);
        let zoomed = s.wanted().unwrap();
        assert_eq!((zoomed.width, zoomed.height), (1250, 938));
        s.zoom_by(100);
        assert_eq!(s.zoom, MAX_ZOOM);
        s.offset = (40, -20);
        s.zoom_by(-100);
        assert_eq!((s.zoom, s.offset), (MIN_ZOOM, (0, 0)));
        s.zoom_by(3);
        s.fit();
        assert_eq!((s.zoom, s.offset), (1.0, (0, 0)));
    }

    #[test]
    fn the_request_never_passes_the_decoders_side_limit() {
        let mut s = ViewerState::new();
        s.open("https://c.test/p".into());
        s.size = (4000, 3000);
        s.zoom_by(100);
        let key = s.wanted().unwrap();
        assert!(key.width <= 8192 && key.height <= 8192);
    }

    #[test]
    fn nothing_is_wanted_before_a_picture_or_a_size() {
        let mut s = ViewerState::new();
        assert!(s.wanted().is_none());
        s.open("https://c.test/p".into());
        assert!(s.wanted().is_none(), "no size yet");
    }

    #[test]
    fn a_zoomed_request_stays_within_the_pixel_budget() {
        let mut state = ViewerState::new();
        state.open("https://cdn.test/a.jpg".into());
        state.size = (960, 640);
        state.zoom = MAX_ZOOM;
        let key = state.wanted().unwrap();
        assert!(u64::from(key.width) * u64::from(key.height) <= MAX_PIXELS, "{key:?}");
        // The proportions of the window are kept.
        assert!((key.width as f32 / key.height as f32 - 1.5).abs() < 0.01, "{key:?}");
        state.zoom = 1.0;
        let fit = state.wanted().unwrap();
        assert_eq!((fit.width, fit.height), (960, 640));
    }
}

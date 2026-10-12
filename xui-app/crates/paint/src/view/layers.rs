#![forbid(unsafe_code)]

//! The Layers panel: one row per layer, top layer first, each with a visibility
//! eye, a thumbnail over a checkerboard and the name.
//!
//! A custom node like the tool grid and the Colors panel rather than xui's
//! `ListView`: a list row holds text only (no picture, no second hit target),
//! and this row needs a thumbnail and an eye that toggles without selecting.
//! xui gap: G18 — `ListView` cells cannot hold images or controls.
//!
//! Thumbnails are cached per layer and rebuilt only when the model's revision
//! moves (and only the layers whose scaled picture actually changed are given
//! to the backend again); see [`thumb`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::widget::{Control, Placeable};

use super::{KeyOwner, Msg};
use crate::model::Model;

mod paint;
mod thumb;

/// One row's design height.
const ROW: Dip = Dip(40.0);
/// The thumbnail box's design side.
const THUMB: Dip = Dip(32.0);
/// The eye's design side.
const EYE: Dip = Dip(16.0);
/// Padding inside a row.
const PAD: Dip = Dip(6.0);

/// What a point of the panel hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    /// A layer's eye (the layer index).
    Eye(usize),
    /// A layer's row (the layer index).
    Row(usize),
}

/// What the panel shows of one layer.
struct Row {
    name: String,
    visible: bool,
    opacity: u8,
    thumb: Option<Image>,
}

struct State {
    bounds: Cell<Rect>,
    dpi: Cell<u32>,
    rows: RefCell<Vec<Row>>,
    active: Cell<usize>,
    /// How far the list is scrolled, in device pixels.
    scroll: Cell<i32>,
    hover: Cell<Option<Hit>>,
    /// The revision and thumbnail size the thumbnails were built for.
    built: Cell<Option<(u64, u32)>>,
    /// The active layer and layer count the list was last scrolled to show, so
    /// a sync that changes neither leaves a wheel scroll alone.
    revealed: Cell<Option<(usize, usize)>>,
}

impl State {
    fn px(&self, dip: Dip) -> i32 {
        dip.to_px(self.dpi.get()).value().max(1)
    }

    fn row_height(&self) -> i32 {
        self.px(ROW)
    }

    /// The row number (0 is the top layer) of layer `layer`.
    fn row_of(&self, layer: usize) -> usize {
        self.rows.borrow().len().saturating_sub(1 + layer)
    }

    /// The rect of layer `layer`'s row, scrolled.
    fn row_rect(&self, layer: usize) -> Rect {
        let bounds = self.bounds.get();
        let top = bounds.top - self.scroll.get() + self.row_of(layer) as i32 * self.row_height();
        Rect::new(bounds.left, top, bounds.right, top + self.row_height())
    }

    fn eye_rect(&self, layer: usize) -> Rect {
        let row = self.row_rect(layer);
        let side = self.px(EYE);
        let left = row.left + self.px(PAD);
        let top = row.top + (row.height() - side) / 2;
        Rect::new(left, top, left + side, top + side)
    }

    fn thumb_box(&self, layer: usize) -> Rect {
        let row = self.row_rect(layer);
        let side = self.px(THUMB);
        let left = self.eye_rect(layer).right + self.px(PAD);
        let top = row.top + (row.height() - side) / 2;
        Rect::new(left, top, left + side, top + side)
    }

    fn name_rect(&self, layer: usize) -> Rect {
        let row = self.row_rect(layer);
        let left = self.thumb_box(layer).right + self.px(PAD);
        Rect::new(
            left,
            row.top,
            (row.right - self.px(PAD)).max(left),
            row.bottom,
        )
    }

    fn content_height(&self) -> i32 {
        self.rows.borrow().len() as i32 * self.row_height()
    }

    fn max_scroll(&self) -> i32 {
        (self.content_height() - self.bounds.get().height()).max(0)
    }

    fn clamp_scroll(&self) {
        self.scroll
            .set(self.scroll.get().clamp(0, self.max_scroll()));
    }

    /// Scrolls just enough to show layer `layer`'s row.
    fn reveal(&self, layer: usize) {
        let view = self.bounds.get();
        let row = self.row_rect(layer);
        if row.top < view.top {
            self.scroll.set(self.scroll.get() - (view.top - row.top));
        } else if row.bottom > view.bottom {
            self.scroll
                .set(self.scroll.get() + (row.bottom - view.bottom));
        }
        self.clamp_scroll();
    }

    fn hit(&self, point: Point) -> Option<Hit> {
        if !self.bounds.get().contains(point) {
            return None;
        }
        let count = self.rows.borrow().len();
        (0..count).find_map(|layer| {
            if self.eye_rect(layer).shrink(-2).contains(point) {
                Some(Hit::Eye(layer))
            } else if self.row_rect(layer).contains(point) {
                Some(Hit::Row(layer))
            } else {
                None
            }
        })
    }
}

/// The Layers panel node.
pub struct LayersPanel {
    control: Control<Msg>,
    state: Rc<State>,
    keys: KeyOwner,
}

impl LayersPanel {
    /// Creates the panel; the window layout places it.
    pub fn new(ui: &Ui<Msg>) -> Result<LayersPanel> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let keys = KeyOwner::new(false);
        let state = Rc::new(State {
            bounds: Cell::new(Rect::default()),
            dpi: Cell::new(ui.dpi()),
            rows: RefCell::new(Vec::new()),
            active: Cell::new(0),
            scroll: Cell::new(0),
            hover: Cell::new(None),
            built: Cell::new(None),
            revealed: Cell::new(None),
        });
        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint::paint(canvas, &state, &theme.get());
            }));
        }
        {
            let state = Rc::clone(&state);
            let ui = ui.clone();
            let id = control.id();
            let keys = keys.clone();
            control.on_events(move |event| {
                keys.follow(event);
                handle(&ui, id, &state, event)
            });
        }
        Ok(LayersPanel {
            control,
            state,
            keys,
        })
    }

    /// Whether the panel has the keyboard.
    pub fn key_owner(&self) -> KeyOwner {
        self.keys.clone()
    }

    /// The panel's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Refreshes the rows from `model`. Thumbnails are rebuilt when its
    /// revision (or the DPI) moved, not on every call.
    pub fn sync(&self, model: &Model) {
        let doc = model.document();
        let bound = self.state.px(THUMB) as u32;
        let key = (model.revision(), bound);
        let stale = self.state.built.get() != Some(key);
        {
            let mut rows = self.state.rows.borrow_mut();
            // The panel lists the top layer first; `rows` is bottom-first so a
            // row's index is its layer's.
            rows.truncate(doc.layers().len());
            for (index, layer) in doc.layers().iter().enumerate() {
                if rows.len() == index {
                    rows.push(Row {
                        name: String::new(),
                        visible: true,
                        opacity: 255,
                        thumb: None,
                    });
                }
                let row = &mut rows[index];
                row.name.clone_from(&layer.name);
                row.visible = layer.visible;
                row.opacity = layer.opacity;
                if stale || row.thumb.is_none() {
                    refresh_thumb(row, &layer.pixels, bound);
                }
            }
        }
        self.state.built.set(Some(key));
        self.state.active.set(doc.active());
        self.state.clamp_scroll();
        let shown = (doc.active(), doc.layers().len());
        if self.state.revealed.replace(Some(shown)) != Some(shown) {
            self.state.reveal(doc.active());
        }
        self.control.invalidate();
    }

    /// The device-pixel rect of layer `layer`'s row, for tests and the probe.
    pub fn row_rect(&self, layer: usize) -> Option<Rect> {
        (layer < self.state.rows.borrow().len()).then(|| self.state.row_rect(layer))
    }

    /// The device-pixel rect of layer `layer`'s eye.
    pub fn eye_rect(&self, layer: usize) -> Option<Rect> {
        (layer < self.state.rows.borrow().len()).then(|| self.state.eye_rect(layer))
    }

    /// How many rows the panel lists.
    pub fn len(&self) -> usize {
        self.state.rows.borrow().len()
    }

    /// Whether the panel lists no layer.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Rebuilds `row`'s thumbnail, keeping the old image (and its backend cache
/// entry) when the scaled picture did not change.
fn refresh_thumb(row: &mut Row, pixels: &crate::model::Bitmap, bound: u32) {
    let ((w, h), rgba) = thumb::pixels(pixels, bound);
    let same = row
        .thumb
        .as_ref()
        .is_some_and(|image| image.size() == (w, h) && image.pixels() == rgba.as_slice());
    if !same {
        row.thumb = Image::from_rgba(w, h, rgba).ok();
    }
}

impl Placeable<Msg> for LayersPanel {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Room for two rows at least; the layout gives the rest.
    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let row = ROW.to_px(constraints.dpi).value().max(1);
        Size::new(0, 2 * row)
    }

    fn placed(&self, ui: &Ui<Msg>, rect: Rect) {
        self.state.bounds.set(rect);
        self.state.dpi.set(ui.dpi());
        self.state.clamp_scroll();
    }
}

fn handle(ui: &Ui<Msg>, id: WidgetId, state: &Rc<State>, event: &Event) -> Option<Msg> {
    let bounds = state.bounds.get();
    let at = |x: i32, y: i32| Point::new(bounds.left + x, bounds.top + y);
    match event {
        Event::MouseMove { x, y, .. } => {
            let hit = state.hit(at(*x, *y));
            if state.hover.get() != hit {
                state.hover.set(hit);
                ui.invalidate(id);
            }
            None
        }
        Event::MouseLeave | Event::CaptureChanged => {
            state.hover.set(None);
            ui.invalidate(id);
            None
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => match state.hit(at(*x, *y))? {
            Hit::Eye(layer) => Some(Msg::LayerEye(layer)),
            Hit::Row(layer) => Some(Msg::LayerSelect(layer)),
        },
        Event::MouseDoubleClick {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => match state.hit(at(*x, *y))? {
            Hit::Row(layer) => Some(Msg::LayerProps(layer)),
            // Two quick clicks on the eye are two toggles, already sent.
            Hit::Eye(_) => None,
        },
        Event::MouseWheel { delta, .. } => {
            // Positive deltas scroll up; a notch is a third of a row.
            let step = state.row_height() / 3 * i32::from(*delta).signum();
            state.scroll.set(state.scroll.get() - step);
            state.clamp_scroll();
            ui.invalidate(id);
            None
        }
        _ => None,
    }
}

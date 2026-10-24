//! The editing canvas: the 32x32 design square drawn large, its polygons and
//! vertices, and the mouse and keys that edit them through the [`Model`].

use std::cell::RefCell;
use std::rc::Rc;

use cursorgfx::theme::DESIGN;
use cursorgfx::{LayerDef, Shape};
use xui_core::app::Ui;
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::message::{Key, MouseButton};
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Theme as UiTheme};

use crate::model::{Model, Tool};
use crate::preview::premultiplied_to_image;
use crate::Msg;

/// Pixels a click may miss a vertex or edge by.
const REACH: i32 = 12;
/// The blank border around the design square, in pixels.
const PAD: i32 = 16;

/// The canvas and its geometry (set when the layout places it).
pub struct View {
    pub model: Rc<RefCell<Model>>,
    /// Top-left of the control in client pixels.
    origin: (i32, i32),
    size: (i32, i32),
    dragging: bool,
    /// The next move of a dragged vertex starts an undo step.
    armed: bool,
    /// The built-in drawing last shown large: shape, side, image, margin.
    shown: RefCell<Option<Shown>>,
}

type Shown = (cursorgfx::Shape, u32, Image, i32);

impl View {
    pub fn new(model: Rc<RefCell<Model>>) -> View {
        View {
            model,
            origin: (0, 0),
            size: (0, 0),
            dragging: false,
            armed: false,
            shown: RefCell::new(None),
        }
    }

    /// Side of the design square on screen and its top-left, in control pixels.
    fn square(&self) -> (i32, i32, i32) {
        let side = (self.size.0.min(self.size.1) - 2 * PAD).max(32);
        ((self.size.0 - side) / 2, (self.size.1 - side) / 2, side)
    }

    fn to_screen(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let (left, top, side) = self.square();
        (left + x * side / DESIGN, top + y * side / DESIGN)
    }

    /// Design units of a client-pixel position.
    fn to_design(&self, (px, py): (i32, i32)) -> (i32, i32) {
        let (left, top, side) = self.square();
        let (x, y) = (px - self.origin.0 - left, py - self.origin.1 - top);
        (x * DESIGN / side, y * DESIGN / side)
    }

    /// Design units spanned by `REACH` pixels.
    fn reach(&self) -> i32 {
        REACH * DESIGN / self.square().2
    }
}

/// The custom-painted control (the `Placeable` the layout sizes).
pub struct CanvasControl {
    pub control: Control<Msg>,
    ui: Ui<Msg>,
    view: Rc<RefCell<View>>,
}

impl CanvasControl {
    pub fn new(ui: &Ui<Msg>, view: Rc<RefCell<View>>) -> Result<CanvasControl> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        {
            let (view, theme) = (Rc::clone(&view), ui.theme_handle());
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(view) = view.try_borrow() {
                    paint(canvas, &view, &theme.get());
                }
            }));
        }
        {
            let (view, ui, id) = (Rc::clone(&view), ui.clone(), control.id());
            control.on_events(move |event| handle(&ui, id, &view, event));
        }
        Ok(CanvasControl {
            control,
            ui: ui.clone(),
            view,
        })
    }

    pub fn invalidate(&self) {
        self.ui.invalidate(self.control.id());
    }
}

impl Placeable<Msg> for CanvasControl {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        let mut view = self.view.borrow_mut();
        view.origin = (rect.left, rect.top);
        view.size = (rect.width(), rect.height());
        drop(view);
        self.ui.invalidate(self.control.id());
    }
}

fn paint(canvas: &mut dyn Canvas, view: &View, theme: &UiTheme) {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);
    let (left, top, side) = view.square();
    let at = |x: i32, y: i32| (bounds.left + x, bounds.top + y);
    let (x0, y0) = at(left, top);
    canvas.fill_rect(
        Rect::new(x0, y0, x0 + side, y0 + side),
        Color::hex(0x808080),
    );
    grid(canvas, (x0, y0), side);
    let model = view.model.borrow();
    match model.def() {
        Some(def) => {
            for layer in &def.layers {
                draw_layer(canvas, view, bounds, layer);
            }
            draw_markers(canvas, view, bounds, &model, def.hot);
        }
        None => builtin(canvas, view, (x0, y0), side, model.shape),
    }
    pending(canvas, view, bounds, &model);
}

fn grid(canvas: &mut dyn Canvas, (x0, y0): (i32, i32), side: i32) {
    for cell in 0..=32 {
        let d = cell * side / 32;
        let shade = if cell % 8 == 0 { 0x6a6a6a } else { 0x777777 };
        canvas.draw_line(
            Point::new(x0 + d, y0),
            Point::new(x0 + d, y0 + side),
            Color::hex(shade),
            1.0,
        );
        canvas.draw_line(
            Point::new(x0, y0 + d),
            Point::new(x0 + side, y0 + d),
            Color::hex(shade),
            1.0,
        );
    }
}

fn point(view: &View, bounds: Rect, p: (i32, i32)) -> Point {
    let (x, y) = view.to_screen(p);
    Point::new(bounds.left + x, bounds.top + y)
}

fn draw_layer(canvas: &mut dyn Canvas, view: &View, bounds: Rect, layer: &LayerDef) {
    let outline = Color::hex(layer.outline);
    for poly in &layer.polys {
        let points: Vec<Point> = poly.iter().map(|&p| point(view, bounds, p)).collect();
        canvas.fill_polygon(&points, Color::hex(layer.fill));
        let width = if layer.thin { 1.0 } else { 2.0 };
        for (i, &from) in points.iter().enumerate() {
            canvas.draw_line(from, points[(i + 1) % points.len()], outline, width);
        }
    }
}

/// Vertex handles (the picked one highlighted) and the hotspot cross.
fn draw_markers(
    canvas: &mut dyn Canvas,
    view: &View,
    bounds: Rect,
    model: &Model,
    hot: (i32, i32),
) {
    let handle = |canvas: &mut dyn Canvas, p: Point, colour: u32| {
        let r = Rect::new(p.x - 4, p.y - 4, p.x + 4, p.y + 4);
        canvas.fill_rect(r, Color::hex(colour));
        canvas.stroke_rect(r, Color::hex(0x000000), 1.0);
    };
    if let Some(def) = model.def() {
        for (l, layer) in def.layers.iter().enumerate() {
            for (pi, poly) in layer.polys.iter().enumerate() {
                for (vi, &v) in poly.iter().enumerate() {
                    let picked = model
                        .selected
                        .is_some_and(|s| (s.layer, s.poly, s.vertex) == (l, pi, vi));
                    let colour = if picked { 0xff9500 } else { 0xffffff };
                    handle(canvas, point(view, bounds, v), colour);
                }
            }
        }
    }
    let c = point(view, bounds, hot);
    let red = Color::hex(0xe02020);
    canvas.draw_line(Point::new(c.x - 9, c.y), Point::new(c.x + 9, c.y), red, 2.0);
    canvas.draw_line(Point::new(c.x, c.y - 9), Point::new(c.x, c.y + 9), red, 2.0);
}

/// The built-in drawing, shown large until the shape is customised. The
/// sprite is rendered once per shape and size, not on every paint.
fn builtin(canvas: &mut dyn Canvas, view: &View, (x0, y0): (i32, i32), side: i32, shape: Shape) {
    let px = side.clamp(8, 256) as u32;
    let mut shown = view.shown.borrow_mut();
    if shown
        .as_ref()
        .is_none_or(|(s, p, ..)| (*s, *p) != (shape, px))
    {
        let sprite = cursorgfx::render(shape, px, 0);
        *shown = premultiplied_to_image(&sprite)
            .map(|image| (shape, px, image, cursorgfx::margin(px) as i32));
    }
    if let Some((_, _, image, margin)) = shown.as_ref() {
        let (w, h) = (image.width() as i32, image.height() as i32);
        let (x, y) = (x0 - margin, y0 - margin);
        canvas.draw_image(image, Rect::new(x, y, x + w, y + h));
    }
}

fn pending(canvas: &mut dyn Canvas, view: &View, bounds: Rect, model: &Model) {
    let blue = Color::hex(0x2a7fff);
    let points: Vec<Point> = model
        .pending
        .iter()
        .map(|&p| point(view, bounds, p))
        .collect();
    for pair in points.windows(2) {
        canvas.draw_line(pair[0], pair[1], blue, 2.0);
    }
    for p in &points {
        canvas.fill_rect(Rect::new(p.x - 3, p.y - 3, p.x + 3, p.y + 3), blue);
    }
}

fn handle(ui: &Ui<Msg>, id: WidgetId, view: &Rc<RefCell<View>>, event: &Event) -> Option<Msg> {
    let mut view = view.borrow_mut();
    match *event {
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            modifiers,
        } => {
            ui.focus(id);
            let at = view.to_design((x, y));
            let reach = view.reach();
            let mut model = view.model.borrow_mut();
            model.snap = !modifiers.alt;
            let changed = press(&mut model, at, reach);
            let drag = model.selected.is_some() && model.tool != Tool::NewPolygon;
            let armed = drag && model.tool == Tool::Select;
            drop(model);
            if drag {
                view.dragging = true;
                view.armed = armed;
                ui.set_capture(id);
            }
            drop(view);
            ui.invalidate(id);
            changed.then_some(Msg::Edited)
        }
        Event::MouseDoubleClick { .. } => {
            let finished = view.model.borrow_mut().finish_polygon();
            drop(view);
            ui.invalidate(id);
            finished.then_some(Msg::Edited)
        }
        Event::MouseMove { x, y, modifiers } if view.dragging => {
            let at = view.to_design((x, y));
            let first = std::mem::take(&mut view.armed);
            let mut model = view.model.borrow_mut();
            model.snap = !modifiers.alt;
            if first {
                model.checkpoint();
            }
            model.move_selected(at);
            drop(model);
            drop(view);
            ui.invalidate(id);
            None
        }
        Event::MouseUp { .. } | Event::CaptureChanged => {
            if std::mem::take(&mut view.dragging) {
                drop(view);
                ui.release_capture();
                ui.invalidate(id);
                return Some(Msg::Edited);
            }
            None
        }
        Event::KeyDown { key, modifiers, .. } => {
            let mut model = view.model.borrow_mut();
            let changed = match key {
                Key::DELETE | Key::BACK => model.delete_selected(),
                Key::RETURN => model.finish_polygon(),
                Key::ESCAPE => {
                    model.cancel_pending();
                    true
                }
                Key::Z if modifiers.ctrl => model.undo(),
                Key::Y if modifiers.ctrl => model.redo(),
                _ => false,
            };
            drop(model);
            drop(view);
            ui.invalidate(id);
            changed.then_some(Msg::Edited)
        }
        _ => None,
    }
}

/// A left press at `at` (design units); `true` when the model changed.
fn press(model: &mut Model, at: (i32, i32), reach: i32) -> bool {
    match model.tool {
        Tool::Select => {
            model.selected = model.hit_vertex(at, reach);
            false
        }
        Tool::AddVertex => model.insert_vertex(at, reach),
        Tool::NewPolygon => {
            model.add_pending(at);
            false
        }
        Tool::Hotspot => {
            model.set_hotspot(at);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Pick;

    fn view() -> View {
        let mut view = View::new(Rc::new(RefCell::new(Model::builtin())));
        view.origin = (100, 50);
        view.size = (600, 500);
        view
    }

    #[test]
    fn client_pixels_and_design_units_round_trip() {
        let view = view();
        let (left, top, side) = view.square();
        for design in [(0, 0), (256, 256), (512, 512), (48, 400)] {
            let (x, y) = view.to_screen(design);
            let back = view.to_design((x + 100, y + 50));
            assert!((back.0 - design.0).abs() <= 2 && (back.1 - design.1).abs() <= 2);
        }
        // The square is centred in the control.
        assert_eq!(left * 2 + side, 600);
        assert_eq!(top * 2 + side, 500);
        assert_eq!(view.to_design((100 + left, 50 + top)), (0, 0));
    }

    #[test]
    fn a_press_acts_by_tool() {
        let mut model = Model::builtin();
        model.pick_shape(Shape::Default);
        let reach = 24;
        // Select: a press near the arrow's tip picks it; elsewhere clears.
        assert!(!press(&mut model, (4, 4), reach));
        assert_eq!(model.selected.map(|p| (p.poly, p.vertex)), Some((0, 0)));
        press(&mut model, (500, 500), reach);
        assert_eq!(model.selected, None);
        // Add vertex: a press on an edge inserts and selects the new point.
        model.tool = Tool::AddVertex;
        let before = model.def().unwrap().layers[0].polys[0].len();
        assert!(press(&mut model, (192, 192), reach));
        assert_eq!(model.def().unwrap().layers[0].polys[0].len(), before + 1);
        assert!(matches!(model.selected, Some(Pick { vertex: 1, .. })));
        // Hotspot: moves the pointer position, snapped.
        model.tool = Tool::Hotspot;
        assert!(press(&mut model, (100, 101), reach));
        assert_eq!(model.def().unwrap().hot, (104, 104));
        // New polygon: points collect until Finish.
        model.tool = Tool::NewPolygon;
        assert!(!press(&mut model, (10, 10), reach));
        assert_eq!(model.pending.len(), 1);
    }
}

//! The doc pane's page as one painted widget.
//!
//! A page used to be a column of a hundred labels and wrapped texts inside the
//! scroll view. `xui`'s scroll view re-measures and re-places such content on
//! every scroll step, and a wrapped text re-shapes all its words each time
//! (35 to 60 ms a step on a big interface page, measured on the host in release:
//! `scroll_tests::survey`), so a thumb drag fell behind the pointer and the
//! pane showed nothing until the button came up. [`DocBody`] lays the blocks
//! out once per width, caches the lines and paints them itself: a scroll step
//! moves one node and repaints only the damaged part of it.
//!
//! The node is a `Label` to the backend, which is what makes the wheel go to the
//! scroll view above it (`backend/wheel.rs`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{
    Canvas, Cursor, Event, NodeKind, NodeSpec, Result, TextStyle, WidgetId,
};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::theme::look::backdrop;
use xui_core::widget::{Control, Placeable};
use xui_core::{Dip, Theme};
use xui_playground::docs::{Block, ChipKind, FieldRow};

use crate::msg::Msg;

/// Longest code line shown whole (longer ones are cut with an ellipsis).
const CODE_COLUMNS: usize = 110;
/// Design pixels: the page's margin, the gap between blocks, between list items,
/// links and chips, and between the columns of a field table.
const PAD: i32 = 10;
const BLOCK_GAP: i32 = 8;
const ITEM_GAP: i32 = 2;
const CHIP_GAP: i32 = 4;
const COLUMN_GAP: i32 = 12;
/// How many widths a page keeps laid out.
const CACHED_WIDTHS: usize = 4;
/// The width used when a layout asks with no bound, in design pixels.
const NATURAL_WIDTH: i32 = 640;

/// Where the window rectangle being repainted is, relative to a node:
/// installed by the app on the LazyOS backend, absent on the host (paint all).
pub type DamageSource = Rc<dyn Fn(WidgetId) -> Option<Rect>>;

thread_local! {
    static DAMAGE: RefCell<Option<DamageSource>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// How many times a page was laid out (a scroll step must not).
    pub static LAYOUTS: Cell<usize> = const { Cell::new(0) };
}

/// Install the source of the repaint rectangle (once, at start).
pub fn set_damage_source(source: DamageSource) {
    DAMAGE.with(|slot| *slot.borrow_mut() = Some(source));
}

fn damage_of(id: WidgetId) -> Option<Rect> {
    DAMAGE.with(|slot| slot.borrow().as_ref().and_then(|source| source(id)))
}

/// How a run of text looks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Face {
    Title,
    Caption,
    Body,
    BodyBold,
    Mono,
    Link,
}

impl Face {
    fn style(self, theme: &Theme) -> TextStyle {
        match self {
            Face::Title => TextStyle::new(theme.text, Dip(20.0)).bold(),
            Face::Caption => TextStyle::new(theme.text_secondary, Dip(11.0)).bold(),
            Face::Body => TextStyle::new(theme.text, Dip(14.0)),
            Face::BodyBold => TextStyle::new(theme.text, Dip(14.0)).weight(700),
            Face::Mono => TextStyle::new(theme.text, Dip(12.0)).family(xui_app::font::MONO_FAMILY),
            Face::Link => TextStyle::new(theme.accent, Dip(14.0)),
        }
    }
}

/// One line of the page, in the page's own pixels.
struct Line {
    text: String,
    rect: Rect,
    face: Face,
}

/// The page laid out for one width and DPI.
struct Laid {
    width: i32,
    dpi: u32,
    height: i32,
    lines: Vec<Line>,
    /// `(rectangle, path)` of each link.
    links: Vec<(Rect, String)>,
}

/// Measured widths and line heights, kept across layouts.
#[derive(Default)]
struct Measures {
    words: HashMap<(Face, u32, String), i32>,
    heights: HashMap<(Face, u32), i32>,
}

struct State {
    ui: Ui<Msg>,
    blocks: Vec<Block>,
    theme: Rc<Cell<Theme>>,
    /// The last few widths laid out, newest first: a scroll view asks for the width
    /// with and without its bar in turn.
    laid: RefCell<Vec<Rc<Laid>>>,
    measures: RefCell<Measures>,
    hover: Cell<Option<usize>>,
}

impl State {
    fn width_of(&self, face: Face, dpi: u32, text: &str) -> i32 {
        let key = (face, dpi, text.to_string());
        if let Some(&width) = self.measures.borrow().words.get(&key) {
            return width;
        }
        let width = self.ui.measure_text(text, &face.style(&Theme::light()), dpi).width;
        self.measures.borrow_mut().words.insert(key, width);
        width
    }

    fn line_height(&self, face: Face, dpi: u32) -> i32 {
        if let Some(&height) = self.measures.borrow().heights.get(&(face, dpi)) {
            return height;
        }
        let height = self.ui.measure_text("Ag", &face.style(&Theme::light()), dpi).height.max(1);
        self.measures.borrow_mut().heights.insert((face, dpi), height);
        height
    }

    /// `text` broken into lines no wider than `width` (a longer word keeps its own line).
    fn wrap(&self, face: Face, dpi: u32, text: &str, width: i32) -> Vec<String> {
        let space = self.width_of(face, dpi, " ");
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut used = 0;
        for word in text.split_whitespace() {
            let w = self.width_of(face, dpi, word);
            if !line.is_empty() && used + space + w > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if !line.is_empty() {
                line.push(' ');
                used += space;
            }
            line.push_str(word);
            used += w;
        }
        if !line.is_empty() || lines.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// The page at `width` pixels (the cache when it was laid out at that width lately).
    fn laid(&self, width: i32, dpi: u32) -> Rc<Laid> {
        let mut cache = self.laid.borrow_mut();
        if let Some(at) = cache.iter().position(|l| l.width == width && l.dpi == dpi) {
            let laid = cache.remove(at);
            cache.insert(0, Rc::clone(&laid));
            return laid;
        }
        drop(cache);
        let laid = Rc::new(self.layout(width, dpi));
        let mut cache = self.laid.borrow_mut();
        cache.insert(0, Rc::clone(&laid));
        cache.truncate(CACHED_WIDTHS);
        laid
    }

    fn layout(&self, width: i32, dpi: u32) -> Laid {
        #[cfg(test)]
        LAYOUTS.with(|n| n.set(n.get() + 1));
        let px = |dip: i32| (dip as f32 * dpi as f32 / 96.0).round() as i32;
        let (pad, gap) = (px(PAD), px(BLOCK_GAP));
        let content = (width - 2 * pad).max(px(40));
        let mut cursor = Cursor2 {
            y: pad,
            lines: Vec::new(),
            links: Vec::new(),
            left: pad,
        };
        let mut first = true;
        for block in &self.blocks {
            if !first {
                cursor.y += gap;
            }
            first = false;
            self.block(block, dpi, content, &mut cursor);
        }
        Laid {
            width,
            dpi,
            height: cursor.y + pad,
            lines: cursor.lines,
            links: cursor.links,
        }
    }

    fn block(&self, block: &Block, dpi: u32, width: i32, at: &mut Cursor2) {
        let px = |dip: i32| (dip as f32 * dpi as f32 / 96.0).round() as i32;
        match block {
            Block::Title(text) => self.text(Face::Title, dpi, text, width, 0, at),
            Block::Heading(text) => self.text(Face::Caption, dpi, &text.to_uppercase(), width, 0, at),
            Block::Paragraph(text) => self.text(Face::Body, dpi, text, width, 0, at),
            Block::Signature(text) => self.one_line(Face::Mono, dpi, &cut(text), at),
            Block::Code { label, text } => {
                self.text(Face::Caption, dpi, &label.to_uppercase(), width, 0, at);
                for line in text.lines() {
                    self.one_line(Face::Mono, dpi, &cut(line), at);
                }
            }
            Block::Fields(rows) => self.fields(rows, dpi, width, at),
            Block::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        at.y += px(ITEM_GAP);
                    }
                    self.text(Face::Body, dpi, &format!("- {item}"), width, 0, at);
                }
            }
            Block::Chips(chips) => {
                for (index, chip) in chips.iter().enumerate() {
                    if index > 0 {
                        at.y += px(CHIP_GAP);
                    }
                    let tag = match chip.kind {
                        ChipKind::Warning => "[!] ",
                        ChipKind::Info => "[i] ",
                    };
                    self.text(Face::BodyBold, dpi, &format!("{tag}{}", chip.text), width, 0, at);
                }
            }
            Block::Links(links) => {
                for (index, link) in links.iter().enumerate() {
                    if index > 0 {
                        at.y += px(ITEM_GAP);
                    }
                    let height = self.line_height(Face::Link, dpi);
                    let w = self.width_of(Face::Link, dpi, &link.label);
                    let rect = Rect::new(at.left, at.y, at.left + w, at.y + height);
                    at.lines.push(Line {
                        text: link.label.clone(),
                        rect,
                        face: Face::Link,
                    });
                    at.links.push((rect, link.path.clone()));
                    at.y += height;
                }
            }
        }
    }

    /// Wrapped `text` at the cursor, `indent` pixels in, `width` wide.
    fn text(&self, face: Face, dpi: u32, text: &str, width: i32, indent: i32, at: &mut Cursor2) {
        let height = self.line_height(face, dpi);
        for line in self.wrap(face, dpi, text, width - indent) {
            let w = self.width_of(face, dpi, &line);
            let rect = Rect::new(at.left + indent, at.y, at.left + indent + w, at.y + height);
            at.lines.push(Line {
                text: line,
                rect,
                face,
            });
            at.y += height;
        }
    }

    fn one_line(&self, face: Face, dpi: u32, text: &str, at: &mut Cursor2) {
        let height = self.line_height(face, dpi);
        let w = self.width_of(face, dpi, text);
        let rect = Rect::new(at.left, at.y, at.left + w, at.y + height);
        at.lines.push(Line {
            text: text.to_string(),
            rect,
            face,
        });
        at.y += height;
    }

    /// Name | type | note columns, each as wide as its widest cell (the note takes the rest).
    fn fields(&self, rows: &[FieldRow], dpi: u32, width: i32, at: &mut Cursor2) {
        let px = |dip: i32| (dip as f32 * dpi as f32 / 96.0).round() as i32;
        let gap = px(COLUMN_GAP);
        let name_w = rows.iter().map(|r| self.width_of(Face::Mono, dpi, &r.name)).max().unwrap_or(0);
        let type_w = rows.iter().map(|r| self.width_of(Face::Mono, dpi, &r.ty)).max().unwrap_or(0);
        let note_at = name_w + gap + type_w + gap;
        let height = self.line_height(Face::Mono, dpi);
        for row in rows {
            let top = at.y;
            for (text, x, w) in [(&row.name, 0, name_w), (&row.ty, name_w + gap, type_w)] {
                let w = w.min(self.width_of(Face::Mono, dpi, text));
                let rect = Rect::new(at.left + x, top, at.left + x + w, top + height);
                at.lines.push(Line {
                    text: text.clone(),
                    rect,
                    face: Face::Mono,
                });
            }
            let mut bottom = top + height;
            if !row.note.is_empty() {
                let mut note = Cursor2 {
                    y: top,
                    lines: Vec::new(),
                    links: Vec::new(),
                    left: at.left,
                };
                self.text(Face::Body, dpi, &row.note, width, note_at, &mut note);
                at.lines.append(&mut note.lines);
                bottom = bottom.max(note.y);
            }
            at.y = bottom;
        }
    }
}

/// Where the next line goes while laying out.
struct Cursor2 {
    y: i32,
    lines: Vec<Line>,
    links: Vec<(Rect, String)>,
    left: i32,
}

fn cut(line: &str) -> String {
    if line.chars().count() > CODE_COLUMNS {
        let mut shown: String = line.chars().take(CODE_COLUMNS - 3).collect();
        shown.push_str("...");
        shown
    } else {
        line.to_string()
    }
}

/// The page as a layout entry: as wide as it is given and as tall as its text.
pub struct DocBody {
    control: Control<Msg>,
    state: Rc<State>,
}

impl DocBody {
    pub fn new(ui: &Ui<Msg>, blocks: Vec<Block>) -> Result<DocBody> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Label, Rect::default()))?;
        let state = Rc::new(State {
            ui: ui.clone(),
            blocks,
            theme: ui.theme_handle(),
            laid: RefCell::default(),
            measures: RefCell::default(),
            hover: Cell::new(None),
        });
        let id = control.id();
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| paint(&state, id, canvas)));
        }
        {
            let state = Rc::clone(&state);
            let ui = ui.clone();
            control.on_events(move |event| on_event(&state, &ui, id, event));
        }
        Ok(DocBody { control, state })
    }

}

fn paint(state: &State, id: WidgetId, canvas: &mut dyn Canvas) {
    let bounds = canvas.bounds();
    let theme = state.theme.get();
    backdrop(canvas, theme.background);
    let laid = state.laid(bounds.width(), canvas.dpi());
    let visible = damage_of(id);
    for line in &laid.lines {
        if let Some(damage) = visible {
            if line.rect.bottom < damage.top - 2 || line.rect.top > damage.bottom + 2 {
                continue;
            }
        }
        let rect = line.rect.offset(bounds.left, bounds.top);
        canvas.draw_text(&line.text, rect, &line.face.style(&theme));
    }
    if let Some(link) = state.hover.get() {
        if let Some((rect, _)) = laid.links.get(link) {
            let rect = rect.offset(bounds.left, bounds.top);
            canvas.draw_line(
                xui_core::Point::new(rect.left, rect.bottom - 1),
                xui_core::Point::new(rect.right, rect.bottom - 1),
                theme.accent,
                1.0,
            );
        }
    }
}

/// The link under `(x, y)`.
fn link_at(state: &State, ui: &Ui<Msg>, id: WidgetId, x: i32, y: i32) -> Option<usize> {
    let width = ui.bounds(id).width();
    if width <= 0 {
        return None;
    }
    state
        .laid(width, ui.dpi())
        .links
        .iter()
        .position(|(rect, _)| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom)
}

fn on_event(state: &State, ui: &Ui<Msg>, id: WidgetId, event: &Event) -> Option<Msg> {
    match event {
        Event::MouseMove { x, y, .. } => {
            let link = link_at(state, ui, id, *x, *y);
            if state.hover.replace(link) != link {
                ui.invalidate(id);
            }
            ui.set_cursor(id, if link.is_some() { Cursor::Hand } else { Cursor::Default });
            None
        }
        Event::MouseLeave | Event::CaptureChanged => {
            if state.hover.replace(None).is_some() {
                ui.invalidate(id);
            }
            None
        }
        Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            let link = link_at(state, ui, id, *x, *y)?;
            let laid = state.laid(ui.bounds(id).width(), ui.dpi());
            let (_, path) = laid.links.get(link)?;
            Some(Msg::OpenLink(path.clone()))
        }
        _ => None,
    }
}

impl Placeable<Msg> for DocBody {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let dpi = constraints.dpi;
        let width = constraints
            .max_width
            .unwrap_or((NATURAL_WIDTH as f32 * dpi as f32 / 96.0) as i32);
        let _ = ui;
        Size::new(width, self.state.laid(width, dpi).height)
    }
}

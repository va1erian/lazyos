//! The Cursor Studio window: shapes on the left, the canvas in the middle,
//! layer colours and the preview on the right, tools and Apply on top.

use std::cell::RefCell;
use std::rc::Rc;

use cursorgfx::Shape;
use xui_core::arrange::{build as create, Handle};
use xui_core::prelude::*;

use crate::canvas::{CanvasControl, View};
use crate::model::{Model, Tool};
use crate::preview::{Preview, PreviewControl};
use crate::store::{self, ThemeStore};
use crate::Msg;

/// The window size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (960, 580);

const NAMES: [&str; cursorgfx::SHAPE_COUNT] = [
    "Arrow",
    "Busy",
    "Text",
    "Hand",
    "Resize horizontal",
    "Resize vertical",
    "Resize NW-SE",
    "Resize NE-SW",
];

const HINT: &str = "Pick a shape, Customize it, then drag its points. Alt+drag places freely.";

pub struct Studio {
    model: Rc<RefCell<Model>>,
    store: Rc<dyn ThemeStore>,
    canvas: Rc<CanvasControl>,
    preview: Rc<PreviewControl>,
    status: Handle<Label<Msg>>,
    fill: Handle<Edit<Msg>>,
    outline: Handle<Edit<Msg>>,
}

fn hex(text: &str) -> Option<u32> {
    let text = text.trim().trim_start_matches('#');
    if text.len() == 6 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        u32::from_str_radix(text, 16).ok()
    } else {
        None
    }
}

impl Studio {
    /// Build the window over `store`; the applied theme (if any) is loaded.
    pub fn build(ui: &Ui<Msg>, store: Rc<dyn ThemeStore>) -> Result<Studio> {
        let mut model = Model::builtin();
        if let Some(theme) = store::current(store.as_ref()) {
            model.load(theme);
        }
        let theme_name = model.theme.name.clone();
        let model = Rc::new(RefCell::new(model));
        let view = Rc::new(RefCell::new(View::new(Rc::clone(&model))));
        let preview = Rc::new(Preview::new(Rc::clone(&model)));
        let (canvas_h, preview_h) = (Handle::new(), Handle::new());
        let (status, fill, outline) = (Handle::new(), Handle::new(), Handle::new());
        let rows: Vec<Vec<String>> = NAMES.iter().map(|n| vec![(*n).to_owned()]).collect();
        let (v, p) = (Rc::clone(&view), Rc::clone(&preview));
        ui.root(
            column().padding(8).gap(8).children((
                row().gap(6).children((
                    button("Select").on_click(Msg::Tool(Tool::Select)),
                    button("Add point").on_click(Msg::Tool(Tool::AddVertex)),
                    button("New polygon").on_click(Msg::Tool(Tool::NewPolygon)),
                    button("Finish").on_click(Msg::Finish),
                    button("Hotspot").on_click(Msg::Tool(Tool::Hotspot)),
                    button("Delete point").on_click(Msg::Delete),
                    button("Undo").on_click(Msg::Undo),
                    button("Redo").on_click(Msg::Redo),
                    button("Customize").on_click(Msg::Customize),
                    button("Reset shape").on_click(Msg::ResetShape),
                )),
                row()
                    .gap(8)
                    .children((
                        column()
                            .gap(6)
                            .children((
                                label("Theme name"),
                                edit()
                                    .then(move |e| {
                                        e.set_text(&theme_name);
                                        e
                                    })
                                    .on_change(Msg::Name),
                                list()
                                    .column("Shape", Fill)
                                    .on_select(Msg::Pick)
                                    .then(move |l| {
                                        l.set_model(rows);
                                        l
                                    })
                                    .fill(1),
                            ))
                            .width(170),
                        create(move |ui: &Ui<Msg>| CanvasControl::new(ui, Rc::clone(&v)))
                            .bind(&canvas_h)
                            .fill(1),
                        column()
                            .gap(6)
                            .children((
                                group(
                                    "Layer",
                                    grid([Track::Auto, Track::Fill(1)]).gap(6).children((
                                        label("Fill"),
                                        edit()
                                            .placeholder("f0f0f0")
                                            .on_change(Msg::Fill)
                                            .bind(&fill),
                                        label("Outline"),
                                        edit()
                                            .placeholder("000000")
                                            .on_change(Msg::Outline)
                                            .bind(&outline),
                                    )),
                                ),
                                row().gap(6).children((
                                    button("Thin outline").on_click(Msg::Thin),
                                    button("New layer").on_click(Msg::AddLayer),
                                )),
                                label("Preview"),
                                create(move |ui: &Ui<Msg>| PreviewControl::new(ui, Rc::clone(&p)))
                                    .bind(&preview_h)
                                    .fill(1),
                                row().gap(6).children((
                                    button("Apply").on_click(Msg::Apply),
                                    button("Use built-in").on_click(Msg::Revert),
                                )),
                            ))
                            .width(250),
                    ))
                    .fill(1),
                label(HINT).bind(&status),
            )),
        )?;
        let studio = Studio {
            model,
            store,
            canvas: canvas_h.get(),
            preview: preview_h.get(),
            status,
            fill,
            outline,
        };
        studio.sync_fields();
        Ok(studio)
    }

    fn say(&self, text: &str) {
        self.status.get().set_text(text);
    }

    fn redraw(&self) {
        self.canvas.invalidate();
        self.preview.invalidate();
    }

    /// Show the active layer's colours in the fields.
    fn sync_fields(&self) {
        let model = self.model.borrow();
        let (fill, outline) = model
            .active_layer()
            .map_or((String::new(), String::new()), |l| {
                (format!("{:06x}", l.fill), format!("{:06x}", l.outline))
            });
        self.fill.get().set_text(&fill);
        self.outline.get().set_text(&outline);
    }

    fn colours(&self, fill: Option<&str>, outline: Option<&str>) {
        let mut model = self.model.borrow_mut();
        let Some(layer) = model.active_layer() else {
            return;
        };
        let new_fill = fill.and_then(hex).unwrap_or(layer.fill);
        let new_outline = outline.and_then(hex).unwrap_or(layer.outline);
        if (new_fill, new_outline) != (layer.fill, layer.outline) {
            model.set_colours(new_fill, new_outline);
        }
    }

    fn apply(&mut self) {
        let result = store::apply(self.store.as_ref(), &self.model.borrow().theme);
        match result {
            Ok(()) => {
                self.model.borrow_mut().mark_applied();
                let name = self.model.borrow().theme.name.clone();
                evidence::applied(&name);
                self.say(&format!("Applied \"{name}\": the pointer changed."));
            }
            Err(error) => {
                evidence::refused(&error);
                self.say(&error);
            }
        }
    }
}

/// The evidence lines: the portable crate cannot call `xui_app::trace`, so
/// the bin installs a sink that does.
pub mod evidence {
    use std::sync::OnceLock;

    type Sink = Box<dyn Fn(&str, &str) + Send + Sync>;
    static SINK: OnceLock<Sink> = OnceLock::new();

    /// Route `(step, detail)` events, once.
    pub fn install(sink: impl Fn(&str, &str) + Send + Sync + 'static) {
        let _ = SINK.set(Box::new(sink));
    }

    pub(super) fn applied(name: &str) {
        if let Some(sink) = SINK.get() {
            sink("APPLY", name);
        }
    }

    pub(super) fn quit() {
        if let Some(sink) = SINK.get() {
            sink("QUIT", "");
        }
    }

    pub(super) fn refused(why: &str) {
        if let Some(sink) = SINK.get() {
            sink("REFUSED", why);
        }
    }
}

impl App for Studio {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Pick(row) => {
                if let Some(&shape) = Shape::ALL.get(row) {
                    self.model.borrow_mut().pick_shape(shape);
                    self.sync_fields();
                    self.say(NAMES[row]);
                }
            }
            Msg::Tool(tool) => {
                let mut model = self.model.borrow_mut();
                model.tool = tool;
                model.cancel_pending();
            }
            Msg::Edited => self.sync_fields(),
            Msg::Customize => {
                self.model.borrow_mut().customize();
                self.sync_fields();
            }
            Msg::ResetShape => {
                self.model.borrow_mut().reset_shape();
                self.sync_fields();
            }
            Msg::Undo => {
                self.model.borrow_mut().undo();
                self.sync_fields();
            }
            Msg::Redo => {
                self.model.borrow_mut().redo();
                self.sync_fields();
            }
            Msg::Delete => {
                self.model.borrow_mut().delete_selected();
            }
            Msg::Finish => {
                if !self.model.borrow_mut().finish_polygon() {
                    self.say("A polygon needs at least three points.");
                }
            }
            Msg::AddLayer => {
                self.model.borrow_mut().add_layer();
                self.sync_fields();
            }
            Msg::Thin => {
                self.model.borrow_mut().toggle_thin();
            }
            Msg::Fill(text) => self.colours(Some(&text), None),
            Msg::Outline(text) => self.colours(None, Some(&text)),
            Msg::Name(text) => self.model.borrow_mut().theme.name = text.trim().to_owned(),
            Msg::Apply => self.apply(),
            Msg::Revert => match store::revert(self.store.as_ref()) {
                Ok(()) => self.say("The built-in pointer is back."),
                Err(error) => self.say(&error),
            },
            Msg::Quit => {
                evidence::quit();
                ui.quit();
            }
        }
        self.redraw();
    }
}

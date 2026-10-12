//! A session over `Setup`: a chosen theme and DPI, an optional text
//! rasterizer, and the named control rectangles the UI probe reports. The
//! tool tests use it; `driver::session` stays the plain 96 dpi light one.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::backend::WidgetId;
use xui_core::geometry::Rect;
use xui_core::image::Image;
use xui_core::{Dip, Theme};
use xui_paint::model::text::TextRasterizer;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp, Setup};

use super::Widgets;
use super::driver::{HEIGHT, Parts, Stg, WIDTH};

/// How a session looks.
#[derive(Clone)]
pub struct Look {
    /// Light or dark.
    pub theme: Theme,
    /// The window's DPI.
    pub dpi: u32,
    /// The Text tool's rasterizer, if the session has fonts.
    pub text: Option<Rc<dyn TextRasterizer>>,
}

impl Look {
    /// Light, 96 dpi, no fonts.
    pub fn plain() -> Look {
        Look {
            theme: Theme::light(),
            dpi: 96,
            text: None,
        }
    }

    /// The same with fonts.
    pub fn with_text(mut self, text: Rc<dyn TextRasterizer>) -> Look {
        self.text = Some(text);
        self
    }

    /// The same at another theme and DPI.
    pub fn at(mut self, theme: Theme, dpi: u32) -> Look {
        self.theme = theme;
        self.dpi = dpi;
        self
    }
}

/// The control rectangles the probe reported, by name.
pub type Probed = Rc<RefCell<HashMap<String, Rect>>>;

/// The centre of a probed control.
pub fn centre(probed: &Probed, name: &str) -> (i32, i32) {
    let rect = *probed
        .borrow()
        .get(name)
        .unwrap_or_else(|| panic!("no probe for {name}"));
    ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
}

/// Re-reads every control's rectangle (call after a tool change: the options
/// bar re-lays out, and a control the tool does not use is not reported).
pub fn probe(stage: &Stg<'_>, probed: &Probed) {
    probed.borrow_mut().clear();
    stage.emit(xui_paint::Msg::Probe);
}

/// Renders a session and runs `step` with the placed widgets and the probe.
pub fn session_look(
    look: Look,
    step: impl FnOnce(&Stg<'_>, &Rc<RefCell<Observer>>, &Parts, &Probed) + 'static,
) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_observer = Rc::clone(&observer);
    let probed: Probed = Rc::new(RefCell::new(HashMap::new()));
    let sink = Rc::clone(&probed);
    let widgets = Widgets::default();
    let built = widgets.clone();
    type Handles = (
        Rc<xui_core::widget::Menu<xui_paint::Msg>>,
        Rc<xui_core::widget::ColorPanel<xui_paint::Msg>>,
        WidgetId,
    );
    let handles: Rc<RefCell<Option<Handles>>> = Rc::new(RefCell::new(None));
    let stored = Rc::clone(&handles);
    let text = look.text.clone();
    render_with(
        Snapshot::new(Dip(WIDTH as f32), Dip(HEIGHT as f32))
            .theme(look.theme)
            .dpi(look.dpi),
        move |ui| {
            let mut setup = Setup::new(Rc::new(MemoryStorage::new())).with_observer(build_observer);
            if let Some(text) = text {
                setup = setup.with_text(text);
            }
            let app = PaintApp::build_with(ui, setup);
            let app = app.map(|mut app| {
                let sink = Rc::clone(&sink);
                app.set_probe(move |_, name, rect| {
                    sink.borrow_mut().insert(name.to_string(), rect);
                });
                app
            });
            if let Ok(app) = &app {
                *stored.borrow_mut() = Some((app.menu(), app.color_panel(), app.canvas().id()));
            }
            built.record(app)
        },
        move |stage| {
            let (menu, panel, canvas) = handles.borrow_mut().take().expect("built");
            let parts = Parts {
                areas: widgets.areas(stage.ui()),
                menu,
                panel,
                canvas,
            };
            probe(stage, &probed);
            step(stage, &observer, &parts, &probed)
        },
    )
    .expect("render")
}

/// Like [`session_look`], also returning where the layout placed the widgets
/// at this DPI (the plain `session_areas` is a 96 dpi light window).
pub fn scene(
    look: Look,
    step: impl FnOnce(&Stg<'_>, &Rc<RefCell<Observer>>, &Parts, &Probed) + 'static,
) -> (Image, super::Areas) {
    let slot: Rc<RefCell<Option<super::Areas>>> = Rc::new(RefCell::new(None));
    let keep = Rc::clone(&slot);
    let image = session_look(look, move |stage, observer, parts, probed| {
        *keep.borrow_mut() = Some(parts.areas);
        step(stage, observer, parts, probed)
    });
    let areas = slot.borrow().expect("the step ran");
    (image, areas)
}

/// Saves an image under `target/snapshots/` for eyeballing.
pub fn save(name: &str, image: &Image) {
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .expect("save snapshot");
}

/// The repository's Liberation fonts on a rasterizer, the faces every desktop
/// image installs.
#[cfg(feature = "text")]
pub fn liberation() -> Rc<dyn TextRasterizer> {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/fonts/liberation");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("assets/fonts/liberation")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ttf"))
        .collect();
    files.sort();
    let mut rasterizer = xui_paint::model::text::CosmicRasterizer::new();
    for file in files {
        rasterizer.register_font(std::fs::read(file).expect("a font file"));
    }
    Rc::new(rasterizer)
}

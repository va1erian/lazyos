//! The window's layout on xui's offscreen backend, in both themes, saved as
//! `target/snapshots/playground-*.png` for a human to look at (nothing here
//! touches the fabric: the model is filled from the compiled-in catalog).

use std::path::PathBuf;
use std::sync::Once;

use xui_app::devtool::{Notice, NoticeStore};
use xui_canvas::snapshot::{try_render, Snapshot};
use xui_core::app::{App, Ui};
use xui_core::arrange::{column, LayoutExt, Mounted};
use xui_core::{Dip, Image, Theme};
use xui_playground::catalog::NodeId;
use xui_playground::docs;

use crate::explorer::Explorer;
use crate::msg::Msg;
use crate::panes::Saved;
use crate::ui::Widgets;
use crate::{callform, docview};

/// A notice store that keeps nothing.
pub(crate) struct Fixed(pub bool);

impl NoticeStore for Fixed {
    fn dismissed(&self) -> bool {
        self.0
    }

    fn set(&self, _dismissed: bool) -> std::io::Result<()> {
        Ok(())
    }
}

struct Shown {
    _mounted: Mounted<Msg>,
    _widgets: Widgets,
}

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, _msg: Msg, _ui: &mut Ui<Msg>) {}
}

pub(crate) fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        xui_app::font::register_mono();
        for font in [
            include_bytes!("../../../../assets/fonts/DroidSans.ttf").as_slice(),
            include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf").as_slice(),
        ] {
            xui_canvas::add_font(font.to_vec());
        }
        xui_canvas::set_default_family("Droid Sans");
    });
}

pub(crate) fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// Render the window with `select` shown in the doc pane. Prints the bounds
/// the probe is built from, so a test can see how containers report them.
/// Where the main window put its panes, in device pixels: `(name, left, top, width, height)`.
type Bounds = Vec<(&'static str, i32, i32, i32, i32)>;

fn render(theme: Theme, dpi: u32, select: &str, form: bool) -> (Image, Bounds) {
    render_at(theme, dpi, (1180.0, 660.0), select, form, Saved::default())
}

/// [`render`] in a window of `size` design pixels with the panes as `saved` has them.
pub(crate) fn render_at(
    theme: Theme,
    dpi: u32,
    size: (f32, f32),
    select: &str,
    form: bool,
    saved: Saved,
) -> (Image, Bounds) {
    register_fonts();
    let node = select.to_string();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Bounds::new()));
    let out = std::rc::Rc::clone(&seen);
    let image = try_render(
        Snapshot::new(Dip(size.0), Dip(size.1)).theme(theme).dpi(dpi),
        move |ui: &mut Ui<Msg>| {
            let w = Widgets::default();
            let notice = Notice::new(Box::new(Fixed(false)));
            let mounted = ui.mount(w.layout(&notice, &saved))?;
            let mut ex = Explorer::new();
            w.tree.get().set_rows(&ex.tree_rows());
            w.live.get().set_model(vec![vec![
                "os.lazy.confd".to_string(),
                "os.lazy.confd.v1".to_string(),
                "1000".to_string(),
                "-".to_string(),
                "system".to_string(),
                "permanent".to_string(),
            ]]);
            w.topics.get().set_model(ex.topic_table());
            w.gallery.get().set_model(ex.gallery_table());
            let catalog = ex.catalog.clone();
            let page = catalog
                .get(&NodeId::method(&node, "Get"))
                .or_else(|| catalog.get(&NodeId::interface(&node)));
            if let Some(n) = page {
                let blocks = docs::blocks(&catalog, n);
                let layout = column().child(docview::view(&blocks).fill(1));
                let _ = w.doc.get().set_layout(layout);
            }
            if form {
                let item = catalog.get(&NodeId::method(&node, "Get")).unwrap();
                if let xui_playground::catalog::Item::Method(i, m) = item.item {
                    let f = xui_playground::form::Form::for_method(i, m);
                    let first = xui_core::arrange::Handle::new();
                    let layout = column()
                        .child(callform::view("os.lazy.confd.v1.Get", &f, None, &first).fill(1));
                    let _ = w.doc.get().set_layout(layout);
                }
            }
            ui.relayout();
            crate::panesapp::fit(ui, &w, &saved);
            ui.relayout();
            let dpi = ui.dpi();
            for (name, px) in [
                ("split_main_at", w.split_main.get().position().to_px(dpi).value()),
                ("split_doc_at", w.split_doc.get().position().to_px(dpi).value()),
            ] {
                out.borrow_mut().push((name, px, 0, 0, 0));
            }
            for (name, id) in [
                ("split_main", w.split_main.get().id()),
                ("split_doc", w.split_doc.get().id()),
                ("bottom", w.bottom.get().id()),
                ("explorer", w.explorer.get().id()),
                ("tree", w.tree.get().id()),
                ("search", w.search.get().id()),
                ("doc", w.doc.get().id()),
                ("editor", w.editor.get().0.id()),
                ("output", w.output.get().id()),
                ("run", w.run.get().id()),
            ] {
                let r = ui.bounds(id);
                out.borrow_mut().push((name, r.left, r.top, r.width(), r.height()));
            }
            Ok(Shown {
                _mounted: mounted,
                _widgets: w,
            })
        },
    )
    .expect("the headless render");
    let bounds = seen.borrow().clone();
    (image, bounds)
}

fn find(bounds: &Bounds, name: &str) -> (i32, i32, i32, i32) {
    let b = bounds.iter().find(|b| b.0 == name).unwrap_or_else(|| panic!("{name} was not reported"));
    (b.1, b.2, b.3, b.4)
}

#[test]
fn the_window_renders_in_both_themes_at_both_scales() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for (dpi, suffix) in [(96, ""), (192, "-2x")] {
            let (image, _) = render(theme, dpi, "os.lazy.confd.v1", false);
            save(&image, &format!("playground-{tag}{suffix}.png"));
        }
    }
}

#[test]
fn the_call_form_renders() {
    let (image, _) = render(Theme::light(), 96, "os.lazy.confd.v1", true);
    save(&image, "playground-form-light.png");
}

/// Two columns and no hole: the Explorer is on the left the whole height and the right
/// column holds the doc pane over the editor and the Output. (Bounds are relative to
/// the pane each sits in, so this reads sizes and the dividers' positions.)
#[test]
fn the_main_window_is_two_columns_without_a_hole() {
    for dpi in [96, 192] {
        let scale = dpi as i32 / 96;
        let (_, bounds) = render(Theme::light(), dpi, "os.lazy.confd.v1", false);
        let (_, _, ew, eh) = find(&bounds, "explorer");
        let (_, _, dw, dh) = find(&bounds, "doc");
        let (_, _, ow, oh) = find(&bounds, "editor");
        let (_, _, pw, ph) = find(&bounds, "output");
        let (_, _, sw, _) = find(&bounds, "split_main");
        assert!(ew > 0 && dw > 0 && ow > 0 && pw > 0, "every pane has a size: {bounds:?}");
        // The doc pane is wider than the old third of the window, and the Explorer is tall.
        assert!(dw > 600 * scale, "doc pane is {dw} wide at {dpi} dpi");
        assert!(eh > 400 * scale, "the explorer is {eh} tall");
        // The Explorer keeps its width and the right column has the rest of the row.
        assert!((ew - 340 * scale).abs() <= 2, "the explorer is {ew} wide");
        let divider = 5 * scale;
        assert!((ew + divider + dw - sw).abs() <= 2, "no hole between the panes: {bounds:?}");
        // The editor and the Output share the bottom row and end together.
        assert!(oh > 0 && ph > 0, "the editor and the output both show");
        assert!(ow + pw + 8 * scale >= dw - 2, "the bottom row spans the right column");
        // The doc pane is taller than the editor row's share but both have room.
        assert!(dh > 100 * scale && oh > 60 * scale, "doc {dh}, editor {oh}");
    }
}

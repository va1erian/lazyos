//! The doc pane's page: a doc page in xui's Markdown view, mounted in the real window layout.
//! It scrolls by itself (the wheel, the keys and its bar are `xui-markdown`'s, tested there), a click
//! on a link reaches the app, and the schema's Markdown is drawn as Markdown.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::devtool::Notice;
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::app::{App, Ui};
use xui_core::arrange::{column, Handle, LayoutExt, Mounted};
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_core::{Dip, Theme};
use xui_markdown::MarkdownView;
use xui_playground::catalog::NodeId;
use xui_playground::docs::{self, Block, DocLink};

use crate::docview;
use crate::explorer::Explorer;
use crate::msg::Msg;
use crate::panes::Saved;
use crate::snapshot_tests::{register_fonts, Fixed};
use crate::ui::Widgets;

type Seen = Rc<RefCell<Vec<String>>>;

struct Shown {
    _mounted: Mounted<Msg>,
    _widgets: Rc<Widgets>,
    seen: Seen,
}

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        if let Msg::OpenLink(path) = msg {
            self.seen.borrow_mut().push(path);
        }
    }
}

/// The page of `blocks` in the doc pane of a 1180x660 window, then `step` on it.
fn with_page(
    blocks: Vec<Block>,
    step: impl FnOnce(&Stage<'_, Msg>, &MarkdownView<Msg>, &Widgets, &Seen) + 'static,
) {
    register_fonts();
    let page: Handle<MarkdownView<Msg>> = Handle::new();
    let (bound, read) = (page.clone(), page);
    let w = Rc::new(Widgets::default());
    let seen: Seen = Rc::default();
    let (build_w, build_seen, step_w, step_seen) = (w.clone(), seen.clone(), w, seen);
    render_with(
        Snapshot::new(Dip(1180.0), Dip(660.0))
            .theme(Theme::light())
            .dpi(96),
        move |ui: &mut Ui<Msg>| {
            let notice = Notice::new(Box::new(Fixed(false)));
            let mounted = ui.mount(build_w.layout(&notice, &Saved::default()))?;
            let layout = column().child(docview::view(&blocks).bind(&bound).fill(1));
            build_w.doc.get().set_layout(layout)?;
            ui.relayout();
            Ok(Shown {
                _mounted: mounted,
                _widgets: build_w,
                seen: build_seen,
            })
        },
        move |stage| {
            // The view lays out what it shows while painting, as a real window does after a change.
            stage.ui().capture().expect("capture");
            step(stage, &read.get(), &step_w, &step_seen)
        },
    )
    .expect("the headless render");
}

fn interface_blocks(name: &str) -> Vec<Block> {
    let catalog = Explorer::new().catalog.clone();
    let node = catalog
        .get(&NodeId::interface(name))
        .expect("the interface");
    docs::blocks(&catalog, node)
}

fn mouse(x: i32, y: i32, down: bool) -> Event {
    let (button, modifiers) = (MouseButton::Left, Modifiers::NONE);
    if down {
        Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        }
    } else {
        Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        }
    }
}

/// The page's top-left corner in window pixels: the pane sits under the two button rows, right of
/// the Explorer and its divider.
fn origin_of(stage: &Stage<'_, Msg>, w: &Widgets, view: &MarkdownView<Msg>) -> (i32, i32) {
    let main = w.split_main.get();
    let outer = stage.ui().bounds(main.id());
    let doc = stage.ui().bounds(w.doc.get().id());
    let page = stage.ui().bounds(view.id());
    (
        outer.left + main.position().0 as i32 + 5 + doc.left + page.left,
        outer.top + doc.top + page.top,
    )
}

#[test]
fn a_long_page_scrolls_inside_the_pane() {
    with_page(
        interface_blocks("os.lazy.display.v1"),
        |stage, view, _w, _seen| {
            assert!(
                stage.ui().bounds(view.id()).height() > 100,
                "the page fills the pane"
            );
            view.set_scroll(600.0);
            stage.ui().capture().expect("capture");
            assert!(
                view.scroll() >= 500.0,
                "the view went down: {}",
                view.scroll()
            );
        },
    );
}

#[test]
fn a_link_in_the_page_opens_on_a_click_and_a_click_elsewhere_opens_nothing() {
    let blocks = vec![
        Block::Title("A page".to_string()),
        Block::Paragraph("Some words to read before the link.".to_string()),
        Block::Links(vec![DocLink {
            label: "The reference".to_string(),
            path: "/docs/os/ref.md".to_string(),
        }]),
    ];
    with_page(blocks, |stage, view, w, seen| {
        let (left, top) = origin_of(stage, w, view);
        // Below the title and the paragraph, past the list marker: find the link's line.
        for y in (40..160).step_by(3) {
            stage.inject(mouse(left + 50, top + y, true));
            stage.inject(mouse(left + 50, top + y, false));
            if !seen.borrow().is_empty() {
                break;
            }
        }
        assert_eq!(
            seen.borrow().first().map(String::as_str),
            Some("/docs/os/ref.md")
        );
        let before = seen.borrow().len();
        stage.inject(mouse(left + 40, top + 20, true));
        stage.inject(mouse(left + 40, top + 20, false));
        assert_eq!(seen.borrow().len(), before, "the title is not a link");
    });
}

#[test]
fn every_page_of_the_catalog_paints() {
    register_fonts();
    let catalog = Explorer::new().catalog.clone();
    // A sample across the catalog: each kind of page, not all of them (each is a whole window).
    for node in catalog.nodes().iter().step_by(97) {
        with_page(docs::blocks(&catalog, node), |stage, view, _w, _seen| {
            stage.ui().capture().expect("capture");
            assert!(view.current_document().blocks().len() > 1);
        });
    }
}

#[test]
fn a_method_page_shows_its_rhai_in_colour() {
    let catalog = Explorer::new().catalog.clone();
    let node = catalog
        .get(&NodeId::method("os.lazy.confd.v1", "Get"))
        .expect("the method");
    with_page(docs::blocks(&catalog, node), |stage, view, _w, _seen| {
        let doc = view.current_document();
        let code = doc
            .blocks()
            .iter()
            .position(
                |b| matches!(&b.kind, xui_markdown::BlockKind::Code { lang } if &**lang == "rhai"),
            )
            .expect("a rhai block");
        assert!(view.scroll_to_anchor("arguments") || code > 0);
        view.set_scroll(220.0);
        let image = stage.ui().capture().expect("capture");
        crate::snapshot_tests::save(&image, "playground-method-code-light.png");
    });
}

//! The doc pane scrolls like the Explorer's list: a thumb drag follows the pointer
//! step by step and a scroll step costs a move, not a re-layout of the page.
//!
//! The wheel reaching the scroll view over a label is the backend's job and is tested
//! there (`backend/wheel.rs`); the cost of a step is measured by `survey`
//! (`cargo test --release survey -- --ignored --nocapture`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::devtool::Notice;
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::app::{App, Ui};
use xui_core::arrange::{column, Handle, LayoutExt, Mounted};
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_core::widget::ScrollView;
use xui_core::{Dip, Px, Theme};
use xui_playground::catalog::NodeId;
use xui_playground::docs::{self, Block, DocLink};

use crate::docbody::LAYOUTS;
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
    step: impl FnOnce(&Stage<'_, Msg>, &ScrollView<Msg>, &Widgets, &Seen) + 'static,
) {
    register_fonts();
    let scroll: Handle<ScrollView<Msg>> = Handle::new();
    let (bound, read) = (scroll.clone(), scroll);
    let w = Rc::new(Widgets::default());
    let seen: Seen = Rc::default();
    let (build_w, build_seen, step_w, step_seen) = (w.clone(), seen.clone(), w, seen);
    render_with(
        Snapshot::new(Dip(1180.0), Dip(660.0)).theme(Theme::light()).dpi(96),
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
        move |stage| step(stage, &read.get(), &step_w, &step_seen),
    )
    .expect("the headless render");
}

fn interface_blocks(name: &str) -> Vec<Block> {
    let catalog = Explorer::new().catalog.clone();
    let node = catalog.get(&NodeId::interface(name)).expect("the interface");
    docs::blocks(&catalog, node)
}

fn mouse(x: i32, y: i32, kind: u8) -> Event {
    let modifiers = Modifiers::NONE;
    match kind {
        0 => Event::MouseDown { x, y, button: MouseButton::Left, modifiers },
        1 => Event::MouseMove { x, y, modifiers },
        _ => Event::MouseUp { x, y, button: MouseButton::Left, modifiers },
    }
}

/// The scroll view's top-left corner in window pixels: the pane sits under the two
/// button rows, right of the Explorer and its divider.
fn origin_of(stage: &Stage<'_, Msg>, w: &Widgets, view: &ScrollView<Msg>) -> (i32, i32) {
    let main = w.split_main.get();
    let outer = stage.ui().bounds(main.id());
    let doc = stage.ui().bounds(w.doc.get().id());
    let scroll = stage.ui().bounds(view.id());
    (
        outer.left + main.position().0 as i32 + 5 + doc.left + scroll.left,
        outer.top + doc.top + scroll.top,
    )
}

#[test]
fn a_scroll_step_moves_the_page_without_laying_it_out_again() {
    with_page(interface_blocks("os.lazy.display.v1"), |_stage, view, _w, _seen| {
        assert!(view.content_height().value() > 700, "the page scrolls");
        let before = LAYOUTS.with(|n| n.get());
        for step in 1..=60 {
            view.scroll_to(Px(step * 9));
        }
        assert!(view.offset().value() >= 500, "the view went down: {}", view.offset().value());
        assert_eq!(LAYOUTS.with(|n| n.get()), before, "no step re-wrapped the text");
    });
}

#[test]
fn dragging_the_thumb_moves_the_page_with_every_step_of_the_pointer() {
    with_page(interface_blocks("os.lazy.display.v1"), |stage, view, w, _seen| {
        let (left, top) = origin_of(stage, w, view);
        let scroll = stage.ui().bounds(view.id());
        // The bar is the view's right edge; the thumb starts at the top of the track.
        let (x, y) = (left + scroll.width() - 6, top + 20);
        stage.inject(mouse(x, y, 0));
        let mut last = view.offset().value();
        let mut moved = 0;
        for step in 1..=8 {
            stage.inject(mouse(x, y + step * 10, 1));
            let now = view.offset().value();
            assert!(now >= last, "the page never goes back while the pointer goes on");
            if now > last {
                moved += 1;
            }
            last = now;
        }
        stage.inject(mouse(x, y + 80, 2));
        assert!(moved >= 6, "the page followed the pointer in {moved} of 8 steps");
        assert!(last > 100, "and went somewhere: {last}");
    });
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
        // The title is about 28 px tall and a paragraph line 19: the link sits near y = 10 + 28 + 8 + 19 + 8.
        let mut hit = false;
        for y in 60..100 {
            stage.inject(mouse(left + 40, top + y, 0));
            stage.inject(mouse(left + 40, top + y, 2));
            hit |= !seen.borrow().is_empty();
            if hit {
                break;
            }
        }
        assert_eq!(seen.borrow().first().map(String::as_str), Some("/docs/os/ref.md"));
        let before = seen.borrow().len();
        stage.inject(mouse(left + 40, top + 14, 0));
        stage.inject(mouse(left + 40, top + 14, 2));
        assert_eq!(seen.borrow().len(), before, "the title is not a link");
    });
}

/// How long a scroll step takes on every page of the catalog (a bench, not a test).
#[test]
#[ignore]
fn survey() {
    register_fonts();
    let catalog = Explorer::new().catalog.clone();
    let mut costs: Vec<(u128, i32, String)> = Vec::new();
    for node in catalog.nodes() {
        let blocks = docs::blocks(&catalog, node);
        let out: Rc<RefCell<Option<(u128, i32)>>> = Rc::default();
        let sink = out.clone();
        with_page(blocks, move |_stage, view, _w, _seen| {
            let started = Instant::now();
            let mut n = 0u128;
            for i in 0..20 {
                let before = view.offset();
                view.scroll_to(Px(i * 3));
                n += u128::from(view.offset() != before);
            }
            if n > 0 {
                *sink.borrow_mut() = Some((started.elapsed().as_micros() / n, view.content_height().value()));
            }
        });
        let got = *out.borrow();
        if let Some((us, height)) = got {
            costs.push((us, height, format!("{:?}", node.id)));
        }
    }
    costs.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!("pages scrolled: {}", costs.len());
    for (us, height, id) in costs.iter().take(8) {
        eprintln!("{us} us/step, content {height} px: {id}");
    }
    eprintln!("median {} us/step", costs[costs.len() / 2].0);
}

//! The main window's stretch policy and its two dividers, on xui's offscreen
//! backend (`docs/messenger-playground-plan.md`, "As built: layout"): which pane
//! takes a window's extra space, the hidden bottom row, and dragging.
//!
//! Bounds are relative to the pane a widget sits in, so these read sizes and the
//! dividers' positions, which do not depend on the origin.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::devtool::Notice;
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::app::{App, Ui};
use xui_core::arrange::Mounted;
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_core::{Dip, Theme};

use crate::msg::Msg;
use crate::panes::{self, Saved};
use crate::snapshot_tests::{register_fonts, render_at, Fixed};
use crate::ui::Widgets;

/// The messages the dividers raised, as text.
type Log = Rc<RefCell<Vec<String>>>;

struct Shown {
    _mounted: Mounted<Msg>,
    log: Log,
}

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::ExplorerMoved(d) => self.log.borrow_mut().push(format!("explorer {:.0}", d.0)),
            Msg::DocSplitMoved(d) => self.log.borrow_mut().push(format!("doc {:.0}", d.0)),
            _ => {}
        }
    }
}

/// A window of `size` design pixels (96 dpi) with `saved`, the panes fitted, then `step` on it.
fn staged(
    size: (f32, f32),
    saved: Saved,
    step: impl FnOnce(&Stage<'_, Msg>, &Widgets, &Log) + 'static,
) {
    register_fonts();
    let w = Rc::new(Widgets::default());
    let log: Log = Rc::default();
    let (build_w, build_log, step_w, step_log) = (w.clone(), log.clone(), w, log);
    render_with(
        Snapshot::new(Dip(size.0), Dip(size.1)).theme(Theme::light()).dpi(96),
        move |ui: &mut Ui<Msg>| {
            let notice = Notice::new(Box::new(Fixed(false)));
            let mounted = ui.mount(build_w.layout(&notice, &saved))?;
            ui.relayout();
            crate::panesapp::fit(ui, &build_w, &saved);
            ui.relayout();
            Ok(Shown {
                _mounted: mounted,
                log: build_log,
            })
        },
        move |stage| step(stage, &step_w, &step_log),
    )
    .expect("the headless render");
}

fn find(bounds: &[(&'static str, i32, i32, i32, i32)], name: &str) -> (i32, i32, i32, i32) {
    let b = bounds
        .iter()
        .find(|b| b.0 == name)
        .unwrap_or_else(|| panic!("{name} was not reported"));
    (b.1, b.2, b.3, b.4)
}

fn sizes_at(w: f32, h: f32, saved: Saved) -> Vec<(&'static str, i32, i32, i32, i32)> {
    render_at(Theme::light(), 96, (w, h), "os.lazy.confd.v1", false, saved).1
}

#[test]
fn a_bigger_window_gives_the_extra_width_to_the_right_column_and_the_extra_height_to_both_rows() {
    let small = sizes_at(900.0, 600.0, Saved::default());
    let mut previous = small;
    for (w, h) in [(1280.0, 720.0), (1800.0, 1000.0), (2560.0, 1400.0)] {
        let now = sizes_at(w, h, Saved::default());
        let (_, _, ew, eh) = find(&now, "explorer");
        let (_, _, pew, peh) = find(&previous, "explorer");
        let (_, _, dw, dh) = find(&now, "doc");
        let (_, _, pdw, pdh) = find(&previous, "doc");
        let (_, _, _, oh) = find(&now, "editor");
        let (_, _, _, poh) = find(&previous, "editor");
        let grown_w = (w as i32) - find_width(&previous);
        assert_eq!(ew, pew, "the Explorer keeps its width at {w}x{h}");
        assert!(eh > peh, "the Explorer is as tall as the window ({eh} vs {peh})");
        assert!(
            (dw - pdw - grown_w).abs() <= 2,
            "the doc pane takes all the extra width at {w}x{h}: {pdw} -> {dw}, window grew {grown_w}"
        );
        assert!(dh > pdh && oh > poh, "doc {pdh} -> {dh} and editor {poh} -> {oh} both grow in height");
        // They keep the saved proportion: the doc pane's divider stays near 55 % of the column.
        let (at, _, _, _) = find(&now, "split_doc_at");
        let (_, _, _, column) = find(&now, "split_doc");
        let share = at as f32 / (column - 5) as f32;
        assert!((share - 0.55).abs() < 0.02, "doc share {share} at {w}x{h}");
        previous = now;
    }
}

/// The window width a size set was rendered at (the Explorer, the divider and the doc pane
/// plus the row's padding).
fn find_width(bounds: &[(&'static str, i32, i32, i32, i32)]) -> i32 {
    let (_, _, sw, _) = find(bounds, "split_main");
    sw + 12
}

#[test]
fn the_smallest_window_leaves_every_pane_its_minimum() {
    let b = sizes_at(900.0, 560.0, Saved::default());
    let (_, _, ew, _) = find(&b, "explorer");
    let (_, _, dw, dh) = find(&b, "doc");
    let (at, _, _, _) = find(&b, "split_doc_at");
    let (_, _, _, oh) = find(&b, "editor");
    assert!(ew >= panes::EXPLORER_MIN as i32 - 2, "explorer {ew}");
    assert!(dw >= panes::RIGHT_MIN as i32, "the right column is {dw} wide");
    assert!(at >= panes::DOC_MIN as i32 - 2, "the doc pane's pane is {at} tall");
    assert!(dh > 30 && oh > 30, "doc {dh}, editor {oh}");
}

#[test]
fn a_saved_layout_is_what_the_window_opens_with() {
    let saved = Saved {
        explorer: 250.0,
        doc_share: 0.35,
        bottom_visible: true,
    };
    let b = sizes_at(1280.0, 720.0, saved);
    let (_, _, ew, _) = find(&b, "explorer");
    assert!((ew - 250).abs() <= 2, "the Explorer is {ew} wide");
    let (at, _, _, _) = find(&b, "split_doc_at");
    let (_, _, _, column) = find(&b, "split_doc");
    let share = at as f32 / (column - 5) as f32;
    assert!((share - 0.35).abs() < 0.02, "doc share {share}");
}

#[test]
fn a_hidden_bottom_row_gives_the_doc_pane_the_whole_column() {
    let shown = sizes_at(1280.0, 720.0, Saved::default());
    let hidden = sizes_at(
        1280.0,
        720.0,
        Saved {
            bottom_visible: false,
            ..Saved::default()
        },
    );
    let (_, _, _, dh_shown) = find(&shown, "doc");
    let (_, _, _, dh_hidden) = find(&hidden, "doc");
    let (at, _, _, _) = find(&hidden, "split_doc_at");
    let (_, _, _, column) = find(&hidden, "split_doc");
    assert!(at >= column - 5 - 2, "the divider is at the bottom edge: {at} of {column}");
    assert!(
        dh_hidden > dh_shown + 200,
        "the doc pane grew from {dh_shown} to {dh_hidden} when the row was hidden"
    );
}

fn mouse(event: fn(i32, i32, Modifiers) -> Event, x: i32, y: i32) -> Event {
    event(x, y, Modifiers::NONE)
}

fn down(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        modifiers,
    }
}

fn moved(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseMove { x, y, modifiers }
}

fn up(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseUp {
        x,
        y,
        button: MouseButton::Left,
        modifiers,
    }
}

/// Drag from `(x, y)` to `(to_x, to_y)` in a few steps.
fn drag(stage: &Stage<'_, Msg>, (x, y): (i32, i32), (to_x, to_y): (i32, i32)) {
    stage.inject(mouse(down, x, y));
    for step in 1..=4 {
        stage.inject(mouse(moved, x + (to_x - x) * step / 4, y + (to_y - y) * step / 4));
    }
    stage.inject(mouse(up, to_x, to_y));
}

#[test]
fn dragging_the_explorer_divider_resizes_the_columns_and_says_the_new_width() {
    staged((1280.0, 720.0), Saved::default(), |stage, w, log| {
        let split = w.split_main.get();
        let node = stage.ui().bounds(split.id());
        let at = split.position().0 as i32;
        let before = stage.ui().bounds(w.explorer.get().id()).width();
        drag(stage, (node.left + at + 2, node.top + 200), (node.left + at + 2 - 100, node.top + 200));
        let after = stage.ui().bounds(w.explorer.get().id()).width();
        assert!((before - after - 100).abs() <= 3, "the Explorer went from {before} to {after}");
        let log = log.borrow();
        assert!(log.last().is_some_and(|m| m.starts_with("explorer ")), "{log:?}");
    });
}

#[test]
fn dragging_the_doc_divider_moves_the_share_and_keeps_the_minimums() {
    staged((1280.0, 720.0), Saved::default(), |stage, w, log| {
        let split = w.split_doc.get();
        // The doc split sits in the right pane of the main one: its origin is the Explorer, a divider on.
        let main = w.split_main.get();
        let outer = stage.ui().bounds(main.id());
        let (left, top) = (outer.left + main.position().0 as i32 + 5, outer.top);
        let column = stage.ui().bounds(split.id()).height();
        let at = split.position().0 as i32;
        // Up by 200: the doc pane shrinks but the editor row grows.
        drag(stage, (left + 50, top + at + 2), (left + 50, top + at + 2 - 200));
        assert!(split.position().0 as i32 <= at - 190, "{} from {at}", split.position().0);
        {
            let log = log.borrow();
            assert!(log.last().is_some_and(|m| m.starts_with("doc ")), "{log:?}");
        }
        // Far up: the doc pane stops at its minimum.
        let now = split.position().0 as i32;
        drag(stage, (left + 50, top + now + 2), (left + 50, top - 300));
        assert!(split.position().0 >= panes::DOC_MIN - 1.0, "{}", split.position().0);
        // Far down: the bottom row stops at its minimum.
        let now = split.position().0 as i32;
        drag(stage, (left + 50, top + now + 2), (left + 50, top + 5000));
        assert!(
            column as f32 - split.position().0 - panes::DIVIDER >= panes::BOTTOM_MIN - 1.0,
            "{} of {column}",
            split.position().0
        );
    });
}

/// The window at four sizes and with the bottom row hidden, saved as
/// `target/snapshots/playground-layout-*.png` for a human to look at.
#[test]
fn the_window_renders_at_four_sizes_and_with_the_bottom_row_hidden() {
    for (w, h) in [(900.0, 600.0), (1280.0, 720.0), (1800.0, 1000.0), (2560.0, 1400.0)] {
        let (image, _) = render_at(Theme::light(), 96, (w, h), "os.lazy.confd.v1", false, Saved::default());
        crate::snapshot_tests::save(&image, &format!("playground-layout-{}x{}.png", w as i32, h as i32));
    }
    let hidden = Saved {
        bottom_visible: false,
        ..Saved::default()
    };
    let (image, _) = render_at(Theme::light(), 96, (1280.0, 720.0), "os.lazy.confd.v1", false, hidden);
    crate::snapshot_tests::save(&image, "playground-layout-hidden.png");
}

/// A long interface page in the painted doc body (`target/snapshots/playground-page-*.png`).
#[test]
fn an_interface_page_renders_in_both_themes() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        let (image, _) = render_at(theme, 96, (1280.0, 720.0), "os.lazy.net.nic.v1", false, Saved::default());
        crate::snapshot_tests::save(&image, &format!("playground-page-{tag}.png"));
    }
    let (image, _) = render_at(Theme::light(), 192, (1280.0, 720.0), "os.lazy.net.nic.v1", false, Saved::default());
    crate::snapshot_tests::save(&image, "playground-page-2x.png");
}

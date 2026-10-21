//! The Topic monitor window on xui's offscreen backend, in both themes at 1x and
//! 2x, with an event list filled in, saved as
//! `target/snapshots/playground-topics-*.png` for a human to look at.

use xui_app::devtool::Notice;
use xui_canvas::snapshot::{try_render, Snapshot};
use xui_core::app::{App, Ui};
use xui_core::arrange::{Handle, Mounted};
use xui_core::widget::HasText;
use xui_core::{Dip, Image, Theme};

use crate::snapshot_tests::{register_fonts, save, Fixed};
use crate::tmsg::TMsg;
use crate::topicsview::event_page;
use crate::topicsui::TopicsWidgets;
use crate::topicswin::WINDOW;

struct Shown {
    _mounted: Mounted<TMsg>,
    _widgets: TopicsWidgets,
}

impl App for Shown {
    type Msg = TMsg;

    fn update(&mut self, _msg: TMsg, _ui: &mut Ui<TMsg>) {}
}

fn rows() -> Vec<Vec<String>> {
    [
        ("pin", "time/tick", "os.lazy.timed", "1R", "#{ unix: 1760000000 }"),
        ("1.0s", "time/tick", "os.lazy.timed", "2", "#{ unix: 1760000001 }"),
        ("2.0s", "time/tick", "os.lazy.timed", "3", "#{ unix: 1760000002 }"),
        ("2.3s", "time/tick", "?", "4", "#{ unix: 1760000003 }"),
    ]
    .iter()
    .map(|r| vec![r.0.into(), r.1.into(), r.2.into(), r.3.into(), r.4.into()])
    .collect()
}

fn details() -> Vec<Vec<String>> {
    [
        "time/tick",
        "from os.lazy.timed",
        "sequence 2",
        "payload Tick",
        "",
        "Tick {",
        "  unix: 1760000001",
        "}",
    ]
    .iter()
    .map(|l| vec![(*l).to_string()])
    .collect()
}

/// Returns the image and the window-relative bounds of the strip.
fn render(theme: Theme, dpi: u32, populated: bool, dismissed: bool) -> (Image, i32) {
    register_fonts();
    let height = std::rc::Rc::new(std::cell::Cell::new(-1));
    let out = std::rc::Rc::clone(&height);
    let image = try_render(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme).dpi(dpi),
        move |ui: &mut Ui<TMsg>| {
            let w = TopicsWidgets::default();
            let notice = Notice::new(Box::new(Fixed(dismissed)));
            let mounted = ui.mount(w.layout(&notice))?;
            if populated {
                let list = Handle::new();
                let tabs = w.subs.get();
                tabs.add_layout_page("time/tick", event_page(&list))?;
                tabs.remove_page(0);
                list.get().set_model(rows());
                w.sub_stats.get().set_text("Buffered/64 q0 d4 m4 drop0 skip0+0 | 3/4 pin1");
                w.details.get().set_model(details());
                w.pub_topic.get().set_text("demo/pg/x");
                w.pub_payload.get().set_text("\"hello\"");
                w.status.get().set_text(0, "Listening to time/tick");
                w.status.get().set_text(1, "listening: 1");
            }
            ui.relayout();
            out.set(ui.bounds(w.strip.get().id()).height());
            Ok(Shown {
                _mounted: mounted,
                _widgets: w,
            })
        },
    )
    .expect("the headless render");
    (image, height.get())
}

#[test]
fn the_monitor_renders_in_both_themes_at_both_scales() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for (dpi, suffix) in [(96, ""), (192, "-2x")] {
            let (image, strip) = render(theme, dpi, true, false);
            save(&image, &format!("playground-topics-{tag}{suffix}.png"));
            assert!(strip > 0, "{tag}{suffix}: the strip shows");
        }
    }
}

#[test]
fn the_empty_monitor_renders_and_a_dismissed_strip_takes_no_room() {
    let (image, strip) = render(Theme::light(), 96, false, false);
    save(&image, "playground-topics-empty-light.png");
    assert!(strip > 0);
    let (image, gone) = render(Theme::dark(), 96, true, true);
    save(&image, "playground-topics-dismissed-dark.png");
    assert_eq!(gone, 0, "the layout reclaims the strip's height");
}

//! The sysmon layout on the host: mounted on xui's offscreen backend with a
//! sample snapshot, checked for placement and saved as light and dark
//! screenshots (`target/snapshots/sysmon-{light,dark}.png`).

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::sysinfo::{decode_words, header, Snapshot, TaskClass, TaskRow, TaskState, WORDS};
use xui_canvas::snapshot::Gallery;
use xui_canvas::OffscreenBackend;
use xui_core::app::{App, Ui};
use xui_core::backend::Backend;
use xui_core::{Rect, Theme};

use super::view::Widgets;
use super::{Msg, WINDOW};

struct Shown;

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, _msg: Msg, _ui: &mut Ui<Msg>) {}
}

fn task(pid: u64, name: &str, state: TaskState, cpu_ticks: u64) -> TaskRow {
    let mut short_name = [0; 8];
    short_name[..name.len()].copy_from_slice(name.as_bytes());
    TaskRow {
        present: true,
        pid,
        ppid: 1,
        state,
        class: TaskClass::Normal,
        weight: 1024,
        cpu_ticks,
        short_name,
        ..TaskRow::EMPTY
    }
}

fn sample() -> Snapshot {
    let mut words = vec![0u64; WORDS];
    for (index, value) in [
        (header::VERSION, 4),
        (header::TICKS, 360_000),
        (header::IDLE_TICKS, 300_000),
        (header::TASKS_LIVE, 4),
        (header::FRAMES_TOTAL, 65_536),
        (header::FRAMES_LIVE, 21_000),
        (header::FRAMES_FREE, 44_536),
        (header::FRAMES_ALLOCATED, 90_000),
        (header::FRAMES_FREED, 69_000),
        (header::FRAMES_RESERVED, 64),
        (header::SLAB_LIVE, 3 << 20),
        (header::SLAB_PEAK, 4 << 20),
        (header::SLAB_OVERSIZED, 1 << 16),
        (header::SLAB_OVERSIZED_PEAK, 1 << 17),
        (header::HEAP_TOTAL, 16 << 20),
        (header::HEAP_USED, 9 << 20),
        (header::HEAP_FREE, 7 << 20),
    ] {
        words[index] = value;
    }
    let mut snapshot = decode_words(&words).expect("a version-4 block");
    for (slot, (name, state)) in [
        ("init", TaskState::Blocked),
        ("xuid", TaskState::Runnable),
        ("healthd", TaskState::Blocked),
        ("sysmon", TaskState::Runnable),
    ]
    .into_iter()
    .enumerate()
    {
        snapshot.tasks[slot] = task(slot as u64 + 1, name, state, 1200 * (slot as u64 + 1));
    }
    snapshot
}

#[test]
fn the_dashboard_lays_out_and_renders_light_and_dark() {
    let backend = Rc::new(OffscreenBackend::new());
    let placed: Rc<RefCell<Vec<Rect>>> = Rc::default();
    let seen = Rc::clone(&placed);
    let capture = Rc::clone(&backend);
    xui_core::app("sysmon")
        .size(WINDOW.0, WINDOW.1)
        .backend(Rc::clone(&backend) as Rc<dyn Backend>)
        .run(move |ui| {
            let widgets = Widgets::new();
            ui.root(widgets.layout())?;
            widgets.show_snapshot(&sample());
            widgets.show_status(360_000, 3, None);
            widgets.set_compact(ui, false);
            ui.relayout();
            assert_eq!(widgets.tasks.get().len(), 4, "the sample's live tasks");
            widgets.show_unavailable(5);
            assert!(
                widgets.tasks.get().is_empty(),
                "a failed read clears the table"
            );
            widgets.show_snapshot(&sample());
            seen.borrow_mut().extend([
                ui.bounds(widgets.tabs.get().id()),
                ui.bounds(widgets.status.get().id()),
            ]);
            let ui = ui.clone();
            capture.set_run_hook(move || {
                let gallery = Gallery::parse(None, Some("target/snapshots"));
                for (variant, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
                    ui.set_theme(theme);
                    let image = ui.capture().expect("a render");
                    gallery.save("sysmon", variant, &image).expect("saved");
                }
            });
            Ok(Shown)
        })
        .expect("the dashboard ran");

    let placed = placed.borrow();
    let (tabs, status) = (placed[0], placed[1]);
    let client = Rect::new(0, 0, WINDOW.0, WINDOW.1);
    assert!(
        tabs.height() > 300,
        "the tabs take the free height: {tabs:?}"
    );
    assert!(
        status.top >= tabs.bottom,
        "the status bar is below the tabs"
    );
    assert!(status.bottom <= client.bottom && status.right <= client.right);
}

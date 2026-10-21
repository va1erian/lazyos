//! The strip, the Help page and the review card built on xui's offscreen
//! backend in both themes and at 1x and 2x, and saved as
//! `target/snapshots/devtool-<what>-{light,dark}[-2x].png` for a human to
//! look at (xui-app/AGENTS-style visual check; the pixels are not diffed).

use std::path::PathBuf;
use std::sync::Once;

use xui_canvas::snapshot::{try_render, Snapshot};
use xui_core::app::{App, Ui};
use xui_core::arrange::{column, label, Handle, LayoutExt, Mounted};
use xui_core::{Dip, Image, Theme};

use super::{
    help_view, review_dialog, strip, HelpPage, Notice, NoticeStore, Provenance, RiskKind, Strip,
};

#[derive(Clone, Debug)]
enum Msg {
    Dismiss,
    Answered,
}

/// Keeps the mounted layout (and the dialog) alive while the frame is drawn.
struct Shown {
    _mounted: Mounted<Msg>,
    _dialog: Option<xui_core::widget::TaskDialog<Msg>>,
}

impl App for Shown {
    type Msg = Msg;

    fn update(&mut self, _msg: Msg, _ui: &mut Ui<Msg>) {}
}

fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for font in [
            include_bytes!("../../../assets/fonts/DroidSans.ttf").as_slice(),
            include_bytes!("../../../assets/fonts/DroidSans-Bold.ttf").as_slice(),
        ] {
            xui_canvas::add_font(font.to_vec());
        }
        xui_canvas::set_default_family("Droid Sans");
    });
}

fn page() -> HelpPage {
    HelpPage::new("Messenger Playground")
        .about("Browse every Messenger interface, write a Rhai script and run it.")
        .does(
            RiskKind::Services,
            "Methods that stop services carry a warning chip.",
        )
        .does(RiskKind::Clipboard, "Scripts are never run unreviewed.")
        .does(
            RiskKind::Flood,
            "Stop ends a run; a message counter shows the rate.",
        )
        .does(
            RiskKind::Script,
            "The review step, the strip, and no auto-run.",
        )
        .extra(
            "Serve a name",
            "Other programs call the script",
            "Listed in the Output tab.",
        )
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// A window of a title label, the strip and (the Help page, or the review card).
fn render(theme: Theme, dpi: u32, review: bool, dismissed: bool) -> (Image, (i32, i32, i32, i32)) {
    register_fonts();
    let seen = std::rc::Rc::new(std::cell::Cell::new((0, 0, 0, 0)));
    let out = std::rc::Rc::clone(&seen);
    let image = try_render(
        Snapshot::new(Dip(660.0), Dip(520.0)).theme(theme).dpi(dpi),
        move |ui: &mut Ui<Msg>| {
            let handle: Handle<Strip<Msg>> = Handle::new();
            let body = if review {
                column().child(label("A script is open in the editor."))
            } else {
                column().child(help_view(&page()).fill(1))
            };
            let mounted = ui.mount(
                column()
                    .child(label("File  Edit  Run  Help"))
                    .child(strip(notice(dismissed), Msg::Dismiss).bind(&handle))
                    .child(body.fill(1)),
            )?;
            let r = ui.bounds(handle.get().id());
            out.set((r.left, r.top, r.width(), r.height()));
            let dialog = if review {
                Some(review_dialog(ui, Provenance::Clipboard, |_| {
                    Some(Msg::Answered)
                })?)
            } else {
                None
            };
            Ok(Shown {
                _mounted: mounted,
                _dialog: dialog,
            })
        },
    )
    .expect("the headless render");
    (image, seen.get())
}

#[test]
fn the_strip_and_the_help_page_render_in_both_themes_at_both_scales() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for (dpi, scale, suffix) in [(96, 1, ""), (192, 2, "-2x")] {
            let (image, (left, _top, width, height)) = render(theme, dpi, false, false);
            save(&image, &format!("devtool-help-{tag}{suffix}.png"));
            assert_eq!(left, 0);
            assert_eq!(
                width,
                660 * scale,
                "{tag}{suffix}: the strip spans the window"
            );
            // One line of 12 dip text plus padding at this scale; the window is
            // wide enough for the whole sentence.
            assert!(
                (16 * scale..=36 * scale).contains(&height),
                "{tag}{suffix}: strip height {height}"
            );
        }
    }
}

#[test]
fn the_review_card_renders_over_the_window() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        let (image, _) = render(theme, 96, true, false);
        save(&image, &format!("devtool-review-{tag}.png"));
    }
}

/// A store that always answers `dismissed` and keeps nothing.
struct Fixed(bool);

impl NoticeStore for Fixed {
    fn dismissed(&self) -> bool {
        self.0
    }

    fn set(&self, _dismissed: bool) -> std::io::Result<()> {
        Ok(())
    }
}

fn notice(dismissed: bool) -> std::rc::Rc<Notice> {
    Notice::new(Box::new(Fixed(dismissed)))
}

#[test]
fn the_strip_with_its_button_and_without_it_after_a_dismissal() {
    for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for (dpi, _scale, suffix) in [(96, 1, ""), (192, 2, "-2x")] {
            let (shown, (_, _, _, height)) = render(theme, dpi, false, false);
            save(&shown, &format!("devtool-strip-{tag}{suffix}.png"));
            assert!(height > 0, "{tag}{suffix}: the strip has a height");
            let (hidden, (_, _, _, gone)) = render(theme, dpi, false, true);
            save(
                &hidden,
                &format!("devtool-strip-dismissed-{tag}{suffix}.png"),
            );
            assert_eq!(gone, 0, "{tag}{suffix}: a dismissed strip takes no height");
        }
    }
}

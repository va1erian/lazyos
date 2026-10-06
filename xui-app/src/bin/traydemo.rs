//! `traydemo` (`os.lazy.traydemo`): the tray sample app (docs/tray-plan.md
//! stage T1). It puts one item on LazyShell's tray through `xui_app::tray`
//! and reports what the shell sends it.
//!
//! Keys (and the buttons) switch the item's picture: `L` a Lucide outline,
//! `P` full-colour pixels at 1x and 2x, `B` a Lucide name that does not exist
//! (the shell falls back to the package icon), `A` toggles `Attention`; `Q`
//! quits.
//!
//! Serial evidence: `TRAYDEMO:UP:PASS` after the first frame,
//! `TRAYDEMO:TRAY:SET:PASS` / `FAIL err=<n>`, `TRAYDEMO:ICON:<kind>`,
//! `TRAYDEMO:ACTIVATE:PASS n=<clicks>`, `TRAYDEMO:SECONDARY:PASS`,
//! `TRAYDEMO:SCROLL:<delta>`, `TRAYDEMO:MENU:<id>:<checked>` and
//! `TRAYDEMO:QUIT:PASS`.

use trayclient::{item, lucide, menu_row, pixels, wire, Event};
use xui_app::launch;
use xui_app::tray::TrayIcon;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (420, 220);
/// How often the tray channel is polled (milliseconds).
const POLL_MILLIS: u32 = 50;
/// The outline the demo shows by default.
const OUTLINE: &str = "zap";
/// A name `lazyicons` does not have: the shell falls back.
const BAD_OUTLINE: &str = "no-such-icon";

#[derive(Clone, Copy)]
enum Msg {
    Tick,
    Icon(Kind),
    Attention,
    Quit,
}

/// Which picture the item shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lucide,
    Pixels,
    Bad,
}

struct Traydemo {
    tray: TrayIcon,
    status: Handle<Label<Msg>>,
    clicks: u32,
    attention: bool,
    /// The menu's check row.
    notify: bool,
    icon: Kind,
}

/// The menu rows' ids.
const ROW_SHOW: u32 = 1;
const ROW_NOTIFY: u32 = 2;
const ROW_ICON: u32 = 4;
const ROW_LUCIDE: u32 = 5;
const ROW_PIXELS: u32 = 6;
const ROW_BAD: u32 = 7;

impl Traydemo {
    fn tooltip(&self) -> String {
        format!("Clicked {} time(s)", self.clicks)
    }

    fn show(&mut self, text: &str) {
        self.status.get().set_text(text);
    }

    /// The item's menu: a default row, a check, a separator and a submenu
    /// of radios that picks the icon (the shell adds Quit).
    fn menu(&self) -> Vec<wire::MenuItem> {
        let mut show = menu_row(ROW_SHOW, "Show window", wire::MENU_KIND_NORMAL);
        show.is_default = true;
        let mut notify = menu_row(ROW_NOTIFY, "Notifications", wire::MENU_KIND_CHECK);
        notify.checked = self.notify;
        let radio = |id, label, kind| {
            let mut row = menu_row(id, label, wire::MENU_KIND_RADIO);
            row.parent = ROW_ICON;
            row.checked = self.icon == kind;
            row
        };
        vec![
            show,
            notify,
            menu_row(3, "", wire::MENU_KIND_SEPARATOR),
            menu_row(ROW_ICON, "Icon", wire::MENU_KIND_SUBMENU),
            radio(ROW_LUCIDE, "Lucide", Kind::Lucide),
            radio(ROW_PIXELS, "Pixels", Kind::Pixels),
            radio(ROW_BAD, "Bad icon", Kind::Bad),
        ]
    }

    fn update_menu(&mut self) {
        let patch = wire::UpdateArgs {
            menu: Some(wire::Menu { rows: self.menu() }),
            ..wire::UpdateArgs::default()
        };
        let _ = self.tray.update(patch);
    }

    fn menu_item(&mut self, id: u32, checked: bool) {
        println!("TRAYDEMO:MENU:{id}:{checked}");
        match id {
            ROW_SHOW => self.show("Shown from the tray menu"),
            ROW_NOTIFY => {
                self.notify = checked;
                self.update_menu();
            }
            ROW_LUCIDE => self.set_icon(Kind::Lucide),
            ROW_PIXELS => self.set_icon(Kind::Pixels),
            ROW_BAD => self.set_icon(Kind::Bad),
            _ => {}
        }
    }

    fn set_icon(&mut self, kind: Kind) {
        self.icon = kind;
        let (icon, name) = match kind {
            Kind::Lucide => (lucide(OUTLINE), "lucide"),
            Kind::Pixels => (pixels(vec![disc(16), disc(32)]), "pixels"),
            Kind::Bad => (lucide(BAD_OUTLINE), "bad"),
        };
        let patch = wire::UpdateArgs {
            icon: Some(icon),
            menu: Some(wire::Menu { rows: self.menu() }),
            ..wire::UpdateArgs::default()
        };
        match self.tray.update(patch) {
            Ok(()) => println!("TRAYDEMO:ICON:{name}"),
            Err(code) => println!("TRAYDEMO:ICON:FAIL err={}", -code),
        }
        self.show(&format!("Icon: {name}"));
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Activate { .. } => {
                self.clicks += 1;
                println!("TRAYDEMO:ACTIVATE:PASS n={}", self.clicks);
                let patch = wire::UpdateArgs {
                    tooltip: Some(self.tooltip()),
                    badge: Some(self.clicks.min(99).to_string()),
                    ..wire::UpdateArgs::default()
                };
                let _ = self.tray.update(patch);
                self.show(&self.tooltip());
            }
            Event::SecondaryActivate { .. } => println!("TRAYDEMO:SECONDARY:PASS"),
            Event::Scroll { delta } => println!("TRAYDEMO:SCROLL:{delta}"),
            Event::MenuItem { id, checked } => self.menu_item(id, checked),
            Event::Ping => {}
        }
    }
}

impl App for Traydemo {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                for event in self.tray.poll() {
                    self.event(event);
                }
            }
            Msg::Icon(kind) => self.set_icon(kind),
            Msg::Attention => {
                self.attention = !self.attention;
                let status = if self.attention {
                    wire::STATUS_ATTENTION
                } else {
                    wire::STATUS_ACTIVE
                };
                let patch = wire::UpdateArgs {
                    status: Some(status),
                    ..wire::UpdateArgs::default()
                };
                let _ = self.tray.update(patch);
                println!("TRAYDEMO:ATTENTION:{}", self.attention);
            }
            Msg::Quit => {
                let _ = self.tray.clear();
                println!("TRAYDEMO:QUIT:PASS");
                ui.quit();
            }
        }
    }
}

/// A `side` x `side` orange disc with a light centre, straight RGBA8: the
/// full-colour picture a pixels icon carries.
fn disc(side: u32) -> (u32, u32, Vec<u8>) {
    let centre = side as f32 / 2.0;
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let dx = x as f32 + 0.5 - centre;
            let dy = y as f32 + 0.5 - centre;
            let distance = (dx * dx + dy * dy).sqrt() / centre;
            let pixel = if distance < 0.35 {
                [0xFF, 0xF2, 0xD0, 0xFF]
            } else if distance < 0.95 {
                [0xF0, 0x7A, 0x1A, 0xFF]
            } else {
                [0, 0, 0, 0]
            };
            data.extend_from_slice(&pixel);
        }
    }
    (side, side, data)
}

fn main() {
    launch::run("TRAYDEMO", "Tray Demo", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("TRAYDEMO:UP:PASS"));
        let mut app = Traydemo {
            tray: TrayIcon::new(),
            status: Handle::default(),
            clicks: 0,
            attention: false,
            notify: false,
            icon: Kind::Lucide,
        };
        let mut item = item(lucide(OUTLINE), "Clicked 0 time(s)");
        item.menu = app.menu();
        match app.tray.set(item) {
            Ok(()) => println!("TRAYDEMO:TRAY:SET:PASS"),
            Err(code) => println!("TRAYDEMO:TRAY:SET:FAIL err={}", -code),
        }
        ui.root(
            column().padding(16).gap(8).children((
                label("Tray Demo").title(),
                label("Click the icon in the taskbar tray.")
                    .bind(&app.status)
                    .fill(1),
                row().gap(8).children((
                    button("Lucide (L)").on_click(Msg::Icon(Kind::Lucide)),
                    button("Pixels (P)").on_click(Msg::Icon(Kind::Pixels)),
                    button("Bad icon (B)").on_click(Msg::Icon(Kind::Bad)),
                    button("Attention (A)").on_click(Msg::Attention),
                )),
            )),
        )?;
        ui.on_key(|key, _| match key {
            Key::L => Some(Msg::Icon(Kind::Lucide)),
            Key::P => Some(Msg::Icon(Kind::Pixels)),
            Key::B => Some(Msg::Icon(Kind::Bad)),
            Key::A => Some(Msg::Attention),
            Key::Q => Some(Msg::Quit),
            _ => None,
        });
        ui.on_close(|| Some(Msg::Quit));
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(POLL_MILLIS);
        Ok(app)
    })
}

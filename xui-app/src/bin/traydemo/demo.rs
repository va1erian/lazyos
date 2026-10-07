//! The Tray Demo's state and what it does with each wake-up, shared by its
//! window and its windowless loop.

use std::cell::RefCell;
use std::rc::Rc;

use trayclient::{item, lucide, menu_row, pixels, wire, Event};
use xui_app::resident::{Resident, Wake};

/// The outline the demo shows by default.
const OUTLINE: &str = "zap";
/// A name `lazyicons` does not have: the shell falls back.
const BAD_OUTLINE: &str = "no-such-icon";

/// The menu rows' ids.
const ROW_SHOW: u32 = 1;
const ROW_NOTIFY: u32 = 2;
const ROW_ICON: u32 = 4;
const ROW_LUCIDE: u32 = 5;
const ROW_PIXELS: u32 = 6;
const ROW_BAD: u32 = 7;

/// Which picture the item shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lucide,
    Pixels,
    Bad,
}

/// What the app should do after a wake-up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Stay,
    /// Open (or keep) the window.
    Show,
    /// Leave: the user or `init` asked to quit.
    Quit,
}

/// Everything the demo remembers across its windows.
pub struct Demo {
    pub res: Rc<Resident>,
    pub clicks: u32,
    pub attention: bool,
    pub notify: bool,
    /// The menu has no rows of its own (`N`): the shell still shows Quit.
    pub bare: bool,
    pub icon: Kind,
    /// The text the window shows under the title.
    pub status: String,
    /// `/tmp/traydemo-ignore-quit` exists: a test app that ignores `Quit`,
    /// so `init` kills it when the grace ends.
    pub ignore_quit: bool,
    /// The window asked the app to quit (`Q`, or `Quit` from `init`).
    pub quitting: bool,
}

pub type Shared = Rc<RefCell<Demo>>;

impl Demo {
    pub fn tooltip(&self) -> String {
        format!("Clicked {} time(s)", self.clicks)
    }

    /// The item's menu: a default row, a check, a separator and a submenu
    /// of radios that picks the icon (the shell adds Quit).
    pub fn menu(&self) -> Vec<wire::MenuItem> {
        if self.bare {
            return Vec::new();
        }
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

    /// Put the whole item on the tray (at start, and after a `Clear`).
    pub fn set_item(&mut self) {
        let mut item = item(self.picture().0, &self.tooltip());
        item.menu = self.menu();
        match self.res.tray.borrow_mut().set(item) {
            Ok(()) => println!("TRAYDEMO:TRAY:SET:PASS"),
            Err(code) => println!("TRAYDEMO:TRAY:SET:FAIL err={}", -code),
        }
    }

    /// Give the menu no rows at all, or its rows back (`N`).
    pub fn toggle_bare(&mut self) {
        self.bare = !self.bare;
        let _ = self.update(wire::UpdateArgs {
            menu: Some(wire::Menu { rows: self.menu() }),
            ..wire::UpdateArgs::default()
        });
        println!("TRAYDEMO:MENU:BARE:{}", self.bare);
    }

    /// Take the item off: a resident app then shows its default item.
    pub fn clear_item(&mut self) {
        match self.res.tray.borrow_mut().clear() {
            Ok(()) => println!("TRAYDEMO:CLEAR:PASS"),
            Err(code) => println!("TRAYDEMO:CLEAR:FAIL err={}", -code),
        }
        self.status = String::from("Item cleared: the tray shows the default item");
    }

    fn picture(&self) -> (wire::Icon, &'static str) {
        match self.icon {
            Kind::Lucide => (lucide(OUTLINE), "lucide"),
            Kind::Pixels => (pixels(vec![disc(16), disc(32)]), "pixels"),
            Kind::Bad => (lucide(BAD_OUTLINE), "bad"),
        }
    }

    fn update(&mut self, patch: wire::UpdateArgs) -> Result<(), i64> {
        self.res.tray.borrow_mut().update(patch)
    }

    pub fn set_icon(&mut self, kind: Kind) {
        self.icon = kind;
        let (icon, name) = self.picture();
        let patch = wire::UpdateArgs {
            icon: Some(icon),
            menu: Some(wire::Menu { rows: self.menu() }),
            ..wire::UpdateArgs::default()
        };
        match self.update(patch) {
            Ok(()) => println!("TRAYDEMO:ICON:{name}"),
            Err(code) => println!("TRAYDEMO:ICON:FAIL err={}", -code),
        }
        self.status = format!("Icon: {name}");
    }

    pub fn toggle_attention(&mut self) {
        self.attention = !self.attention;
        let status = if self.attention {
            wire::STATUS_ATTENTION
        } else {
            wire::STATUS_ACTIVE
        };
        let _ = self.update(wire::UpdateArgs {
            status: Some(status),
            ..wire::UpdateArgs::default()
        });
        println!("TRAYDEMO:ATTENTION:{}", self.attention);
    }

    /// Handle one wake-up; `windowed` says whether a window is open.
    pub fn wake(&mut self, wake: Wake, windowed: bool) -> Flow {
        match wake {
            Wake::Reopen(args) => {
                println!("TRAYDEMO:REOPEN:PASS via=reopen args={args}");
                self.status = String::from("Opened again");
                // A cleared item comes back with the window.
                if self.res.tray.borrow().channel().is_none() {
                    self.set_item();
                }
                Flow::Show
            }
            Wake::Quit(grace_ms) if self.ignore_quit => {
                println!("TRAYDEMO:QUIT:IGNORED grace_ms={grace_ms}");
                Flow::Stay
            }
            Wake::Quit(grace_ms) => {
                println!("TRAYDEMO:QUIT:REQUESTED grace_ms={grace_ms}");
                Flow::Quit
            }
            Wake::Tray(event) => self.tray_event(event, windowed),
        }
    }

    fn tray_event(&mut self, event: Event, windowed: bool) -> Flow {
        match event {
            Event::Activate { .. } => {
                self.clicks += 1;
                println!("TRAYDEMO:ACTIVATE:PASS n={}", self.clicks);
                let patch = wire::UpdateArgs {
                    tooltip: Some(self.tooltip()),
                    badge: Some(self.clicks.min(99).to_string()),
                    ..wire::UpdateArgs::default()
                };
                let _ = self.update(patch);
                self.status = self.tooltip();
                if windowed {
                    Flow::Stay
                } else {
                    println!("TRAYDEMO:REOPEN:PASS via=activate");
                    Flow::Show
                }
            }
            Event::SecondaryActivate { .. } => {
                println!("TRAYDEMO:SECONDARY:PASS");
                Flow::Stay
            }
            Event::Scroll { delta } => {
                println!("TRAYDEMO:SCROLL:{delta}");
                Flow::Stay
            }
            Event::MenuItem { id, checked } => self.menu_item(id, checked),
            Event::Ping => Flow::Stay,
        }
    }

    fn menu_item(&mut self, id: u32, checked: bool) -> Flow {
        println!("TRAYDEMO:MENU:{id}:{checked}");
        match id {
            ROW_SHOW => return Flow::Show,
            ROW_NOTIFY => {
                self.notify = checked;
                let _ = self.update(wire::UpdateArgs {
                    menu: Some(wire::Menu { rows: self.menu() }),
                    ..wire::UpdateArgs::default()
                });
            }
            ROW_LUCIDE => self.set_icon(Kind::Lucide),
            ROW_PIXELS => self.set_icon(Kind::Pixels),
            ROW_BAD => self.set_icon(Kind::Bad),
            _ => {}
        }
        Flow::Stay
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

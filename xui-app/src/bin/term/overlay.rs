//! Input for the layers over the grid: which one answers a key or a click
//! (a prompt, the preferences, the context menu, the find bar, in that order),
//! and what each answer does. The layers' models are pure (`menu.rs`,
//! `prefs.rs`, `findbar.rs`, `prompt.rs`); this routes events into them and
//! carries out what they decide.

use xui_app::probe;
use xui_core::app::Ui;
use xui_core::{Key, Modifiers, MouseButton, Rect};

use super::findbar::{self, Step as FindStep};
use super::menu::{self, Item, Menu, Step as MenuStep};
use super::msg::Msg;
use super::pointer::{MouseKind, MouseMsg};
use super::prefs::{self, Outcome, Prefs};
use super::prompt::{self, Kind};
use super::{conf, Terminal};

impl Terminal {
    /// Whether a layer that takes the keyboard is up.
    pub(super) fn modal(&self) -> bool {
        self.chrome.borrow().modal()
    }

    /// Repaint everything: the layers change what the whole window shows.
    pub(super) fn redraw(&mut self, ui: &mut Ui<Msg>) {
        self.shown = super::view::Shown::default();
        self.repaint(ui);
    }

    /// Print a probe rectangle for session scripts.
    pub(super) fn probe_rect(&self, name: &str, r: Rect) {
        probe::widget(&self.title, name, r.left, r.top, r.width(), r.height());
    }

    fn scale(&self) -> i32 {
        self.look.borrow().metrics.scale
    }

    /// Typed text for the layer that takes it.
    pub(super) fn modal_char(&mut self, ui: &mut Ui<Msg>, ch: char) {
        // Enter, Tab and Backspace arrive as keys too, and are handled there.
        if ch.is_control() {
            return;
        }
        let mut chrome = self.chrome.borrow_mut();
        if chrome.prompt.is_some() || chrome.menu.is_some() {
            return;
        }
        if let Some(prefs) = chrome.prefs.as_mut() {
            let out = prefs.char(ch);
            drop(chrome);
            self.prefs_outcome(ui, out);
            return;
        }
        let edited = chrome.find.as_mut().map(|f| f.insert(ch));
        drop(chrome);
        if edited == Some(FindStep::Edited) {
            self.find_edited(ui);
        }
    }

    /// A key for the topmost layer.
    pub(super) fn modal_key(&mut self, ui: &mut Ui<Msg>, key: Key, mods: Modifiers) {
        self.touch(ui);
        enum Top {
            Prompt,
            Prefs,
            Menu,
            Find,
        }
        let top = {
            let c = self.chrome.borrow();
            if c.prompt.is_some() {
                Top::Prompt
            } else if c.prefs.is_some() {
                Top::Prefs
            } else if c.menu.is_some() {
                Top::Menu
            } else {
                Top::Find
            }
        };
        match top {
            Top::Prompt => match key {
                Key::RETURN => self.answer_prompt(ui, true),
                Key::ESCAPE => self.answer_prompt(ui, false),
                _ => {}
            },
            Top::Prefs => {
                let out = {
                    let mut c = self.chrome.borrow_mut();
                    c.prefs.as_mut().map(|p| p.key(key, mods))
                };
                if let Some(out) = out {
                    self.prefs_outcome(ui, out);
                }
            }
            Top::Menu => {
                if super::keys::is_menu_chord(key, mods) {
                    self.close_menu(ui);
                    return;
                }
                let step = self.chrome.borrow_mut().menu.as_mut().map(|m| m.key(key));
                match step {
                    Some(MenuStep::Choose(item)) => self.choose(ui, item),
                    Some(MenuStep::Close) => self.close_menu(ui),
                    _ => self.redraw(ui),
                }
            }
            Top::Find => {
                if super::keys::is_find_chord(key, mods) {
                    // Ctrl+Shift+F again closes it.
                    self.close_find(ui);
                    return;
                }
                let step = self.chrome.borrow_mut().find.as_mut().map(|f| f.key(key, mods));
                self.find_step(ui, step.unwrap_or(FindStep::None));
            }
        }
    }

    /// Answer the confirmation bar.
    pub(super) fn answer_prompt(&mut self, ui: &mut Ui<Msg>, accept: bool) {
        let kind = self.chrome.borrow().prompt.as_ref().map(|p| p.kind.clone());
        match kind {
            Some(Kind::Paste) => self.confirm(ui, accept),
            Some(Kind::Link(url)) => {
                self.chrome.borrow_mut().prompt = None;
                self.redraw(ui);
                if accept {
                    self.open_url(&url);
                }
            }
            Some(Kind::Close) => {
                self.chrome.borrow_mut().prompt = None;
                self.redraw(ui);
                if accept {
                    println!("TERM:CLOSE:FORCE");
                    self.finish(ui);
                } else {
                    println!("TERM:CLOSE:CANCEL");
                }
            }
            None => {}
        }
    }

    /// A pointer event while a layer is up; whether it was taken.
    pub(super) fn modal_mouse(&mut self, ui: &mut Ui<Msg>, m: &MouseMsg) -> bool {
        let Some(layout) = self.layout.get() else {
            return false;
        };
        let scale = self.scale();
        let down = matches!(m.kind, MouseKind::Down(MouseButton::Left) | MouseKind::Double(MouseButton::Left));
        if self.chrome.borrow().prompt.is_some() {
            if down {
                match prompt::button_at(&layout, m.x, m.y) {
                    Some(accept) => self.answer_prompt(ui, accept),
                    None => {}
                }
            }
            return true;
        }
        if self.chrome.borrow().prefs.is_some() {
            if down {
                let part = {
                    let c = self.chrome.borrow();
                    let prefs = c.prefs.as_ref().expect("prefs");
                    let laid = prefs::layout(prefs, (layout.width, layout.height), scale);
                    if super::geometry::contains(laid.panel, m.x, m.y) {
                        Some(laid.part_at(m.x, m.y).cloned())
                    } else {
                        None
                    }
                };
                match part {
                    Some(Some(part)) => {
                        let out = self.chrome.borrow_mut().prefs.as_mut().map(|p| p.click_part(part));
                        if let Some(out) = out {
                            self.prefs_outcome(ui, out);
                        }
                    }
                    Some(None) => {}
                    // A click outside the panel does nothing: it is modal.
                    None => {}
                }
            }
            return true;
        }
        if self.chrome.borrow().menu.is_some() {
            let step = {
                let mut c = self.chrome.borrow_mut();
                let menu = c.menu.as_mut().expect("menu");
                let laid = menu::layout(menu, (layout.width, layout.height), scale);
                match m.kind {
                    MouseKind::Move => {
                        let before = menu.selected;
                        menu.hover(&laid, m.x, m.y);
                        (menu.selected != before).then_some(MenuStep::None)
                    }
                    MouseKind::Down(MouseButton::Left | MouseButton::Right) => Some(menu.click(&laid, m.x, m.y)),
                    _ => None,
                }
            };
            match step {
                Some(MenuStep::Choose(item)) => self.choose(ui, item),
                Some(MenuStep::Close) => self.close_menu(ui),
                Some(MenuStep::None) => self.redraw(ui),
                None => {}
            }
            return true;
        }
        // The find bar: only clicks on the bar are its own; the grid stays live.
        if self.chrome.borrow().find.is_some() && down {
            if let Some(step) = findbar::click(&layout, m.x, m.y) {
                self.find_step(ui, step);
                return true;
            }
            if super::geometry::contains(findbar::rects(&layout).bar, m.x, m.y) {
                return true;
            }
        }
        false
    }

    /// Open the context menu at (`x`, `y`) with the link under it, if any.
    pub(super) fn open_menu(&mut self, ui: &mut Ui<Msg>, at: (i32, i32), link: Option<String>) {
        let has_selection = self.term.borrow().selection_text().is_some();
        let menu = Menu::new(at, has_selection, link);
        if let Some(layout) = self.layout.get() {
            let laid = menu::layout(&menu, (layout.width, layout.height), self.scale());
            for (entry, row) in menu.entries.iter().zip(&laid.rows) {
                self.probe_rect(&entry.item.probe(), *row);
            }
        }
        self.chrome.borrow_mut().menu = Some(menu);
        self.redraw(ui);
    }

    pub(super) fn close_menu(&mut self, ui: &mut Ui<Msg>) {
        self.chrome.borrow_mut().menu = None;
        self.redraw(ui);
    }

    /// Carry out a chosen entry (`TERM:MENU:<item>`).
    pub(super) fn choose(&mut self, ui: &mut Ui<Msg>, item: Item) {
        let link = self.chrome.borrow().menu.as_ref().and_then(|m| m.link.clone());
        self.chrome.borrow_mut().menu = None;
        println!("TERM:MENU:{}", item.evidence());
        match item {
            Item::Copy => self.copy(ui),
            Item::Paste => self.paste(ui),
            Item::SelectAll => self.select_all(ui),
            Item::Find => self.open_find(ui),
            Item::OpenLink => {
                if let Some(url) = link {
                    self.ask_open(ui, &url);
                }
            }
            Item::CopyLink => {
                if let Some(url) = link {
                    ui.set_clipboard_text(&url);
                }
            }
            Item::ClearScrollback => self.clear_scrollback(),
            Item::Reset => self.reset_terminal(),
            Item::SaveOutput => self.open_save(),
            Item::NewWindow => self.new_window(),
            Item::Preferences => self.open_prefs(ui),
            Item::Close => self.request_close(ui),
        }
        self.redraw(ui);
    }

    /// The preferences panel (Ctrl+,): the families offered are the ones the
    /// Terminal registered at start.
    pub(super) fn open_prefs(&mut self, ui: &mut Ui<Msg>) {
        let prefs = Prefs::new(&self.conf.settings, self.fonts.catalog.names().into_iter().map(String::from).collect());
        if let Some(layout) = self.layout.get() {
            let laid = prefs::layout(&prefs, (layout.width, layout.height), self.scale());
            self.probe_rect("prefs_panel", laid.panel);
            for (_, rect, name) in &laid.parts {
                self.probe_rect(name, *rect);
            }
        }
        self.chrome.borrow_mut().prefs = Some(prefs);
        self.redraw(ui);
    }

    /// What an edit in the preferences asked for: write each key to `confd`
    /// (where the settings follow it like any other writer's), apply it now,
    /// and close if asked.
    pub(super) fn prefs_outcome(&mut self, ui: &mut Ui<Msg>, out: Outcome) {
        let mut settings = self.conf.settings.clone();
        for change in &out.changes {
            self.follower.write(change.key, &change.value);
            settings.apply(change.key, &change.value);
            println!("TERM:PREFS:SET:{}", change.key);
        }
        if !out.changes.is_empty() {
            let motion_none = self.conf.motion_none;
            self.apply_conf(ui, conf::Conf { settings, motion_none });
        }
        if out.close {
            self.chrome.borrow_mut().prefs = None;
        }
        self.redraw(ui);
    }
}

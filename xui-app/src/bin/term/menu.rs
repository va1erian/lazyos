//! The context menu: a panel of commands at the pointer (right click, or
//! Shift+F10 / the Menu key at the cursor), drawn by the Terminal inside its
//! window. It is a model (entries, which are enabled, the selected one) with
//! its geometry and keys as pure functions, and a painter, so it is
//! host-tested and snapshotted; every entry has a probe name
//! (`menu:<label>`) for session scripts.
//!
//! It is drawn here rather than with xui's `Menu` popup because the Terminal
//! needs the entries' rectangles for the UI probe, a menu that stays inside
//! its own window and follows the terminal's colours, and the same
//! keyboard routing as its other overlays.

use xui_core::{Canvas, Color, Key, Rect};

use lazyterm::Palette;

use super::geometry::contains;

/// One command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Copy,
    Paste,
    SelectAll,
    Find,
    OpenLink,
    CopyLink,
    ClearScrollback,
    Reset,
    SaveOutput,
    NewWindow,
    Preferences,
    Close,
}

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            Item::Copy => "Copy",
            Item::Paste => "Paste",
            Item::SelectAll => "Select All",
            Item::Find => "Find...",
            Item::OpenLink => "Open Link",
            Item::CopyLink => "Copy Link",
            Item::ClearScrollback => "Clear Scrollback",
            Item::Reset => "Reset",
            Item::SaveOutput => "Save Output...",
            Item::NewWindow => "New Window",
            Item::Preferences => "Preferences...",
            Item::Close => "Close",
        }
    }

    /// The name in `TERM:MENU:<item>`.
    pub fn evidence(self) -> &'static str {
        match self {
            Item::Copy => "copy",
            Item::Paste => "paste",
            Item::SelectAll => "select_all",
            Item::Find => "find",
            Item::OpenLink => "open_link",
            Item::CopyLink => "copy_link",
            Item::ClearScrollback => "clear_scrollback",
            Item::Reset => "reset",
            Item::SaveOutput => "save_output",
            Item::NewWindow => "new_window",
            Item::Preferences => "preferences",
            Item::Close => "close",
        }
    }

    /// The probe name of the entry's rectangle (no spaces: the probe line puts
    /// the window title after it).
    pub fn probe(self) -> String {
        format!("menu:{}", self.evidence())
    }

    /// The right-hand hint: the shortcut.
    pub fn shortcut(self) -> &'static str {
        match self {
            Item::Copy => "Ctrl+Shift+C",
            Item::Paste => "Ctrl+Shift+V",
            Item::SelectAll => "Ctrl+Shift+A",
            Item::Find => "Ctrl+Shift+F",
            Item::NewWindow => "Ctrl+Shift+N",
            Item::Preferences => "Ctrl+,",
            _ => "",
        }
    }
}

/// The entries in order, with whether a separator precedes each.
const ORDER: [(Item, bool); 12] = [
    (Item::Copy, false),
    (Item::Paste, false),
    (Item::SelectAll, false),
    (Item::Find, true),
    (Item::OpenLink, false),
    (Item::CopyLink, false),
    (Item::ClearScrollback, true),
    (Item::Reset, false),
    (Item::SaveOutput, false),
    (Item::NewWindow, true),
    (Item::Preferences, false),
    (Item::Close, false),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub item: Item,
    pub enabled: bool,
    pub separator: bool,
}

/// What a key did to the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Nothing the menu cares about.
    None,
    /// Close it.
    Close,
    Choose(Item),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    /// Where it was asked for (window pixels).
    pub at: (i32, i32),
    pub entries: Vec<Entry>,
    pub selected: Option<usize>,
    /// The link under the pointer (its target), when there is one.
    pub link: Option<String>,
}

impl Menu {
    /// The menu for the state of the terminal: Copy needs a selection, the
    /// link entries a link.
    pub fn new(at: (i32, i32), has_selection: bool, link: Option<String>) -> Menu {
        let entries = ORDER
            .iter()
            .map(|&(item, separator)| Entry {
                item,
                separator,
                enabled: match item {
                    Item::Copy => has_selection,
                    Item::OpenLink | Item::CopyLink => link.is_some(),
                    _ => true,
                },
            })
            .collect();
        Menu { at, entries, selected: None, link }
    }

    #[cfg(test)]
    pub fn enabled(&self, item: Item) -> bool {
        self.entries.iter().any(|e| e.item == item && e.enabled)
    }

    /// Move the selection by one entry, skipping disabled ones; it does not
    /// wrap.
    fn step(&mut self, forward: bool) {
        let count = self.entries.len();
        let mut at = self.selected;
        loop {
            at = match (at, forward) {
                (None, true) => Some(0),
                (None, false) => Some(count - 1),
                (Some(i), true) if i + 1 < count => Some(i + 1),
                (Some(i), false) if i > 0 => Some(i - 1),
                _ => return,
            };
            if at.is_some_and(|i| self.entries[i].enabled) {
                self.selected = at;
                return;
            }
        }
    }

    /// A key while the menu is open.
    pub fn key(&mut self, key: Key) -> Step {
        match key {
            Key::ESCAPE => Step::Close,
            Key::DOWN => {
                self.step(true);
                Step::None
            }
            Key::UP => {
                self.step(false);
                Step::None
            }
            Key::HOME => {
                self.selected = None;
                self.step(true);
                Step::None
            }
            Key::END => {
                self.selected = None;
                self.step(false);
                Step::None
            }
            Key::RETURN | Key::SPACE => match self.selected {
                Some(i) if self.entries[i].enabled => Step::Choose(self.entries[i].item),
                _ => Step::None,
            },
            _ => Step::None,
        }
    }

    /// The pointer is over (`x`, `y`): select the entry there.
    pub fn hover(&mut self, layout: &MenuLayout, x: i32, y: i32) {
        if let Some(i) = layout.entry_at(x, y) {
            if self.entries[i].enabled {
                self.selected = Some(i);
            }
        }
    }

    /// A click at (`x`, `y`): the entry chosen, `Close` outside the panel, and
    /// nothing on a disabled entry or a separator.
    pub fn click(&self, layout: &MenuLayout, x: i32, y: i32) -> Step {
        if !contains(layout.panel, x, y) {
            return Step::Close;
        }
        match layout.entry_at(x, y) {
            Some(i) if self.entries[i].enabled => Step::Choose(self.entries[i].item),
            _ => Step::None,
        }
    }
}

/// Where the menu is: the panel and each entry's row (window pixels).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLayout {
    pub panel: Rect,
    pub rows: Vec<Rect>,
}

impl MenuLayout {
    pub fn entry_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows.iter().position(|r| contains(*r, x, y))
    }
}

/// The row height, panel width and padding at UI `scale`.
const ROW: i32 = 24;
const WIDTH: i32 = 270;
const PAD: i32 = 4;
const SEPARATOR: i32 = 9;

/// Lay `menu` out inside a window of `window` pixels at `scale`: the panel
/// opens down and right of the point, shifted to stay inside the window.
pub fn layout(menu: &Menu, window: (i32, i32), scale: i32) -> MenuLayout {
    let s = scale.max(1);
    let (w, row) = (WIDTH * s, ROW * s);
    let height = menu
        .entries
        .iter()
        .map(|e| row + if e.separator { SEPARATOR * s } else { 0 })
        .sum::<i32>()
        + 2 * PAD * s;
    let left = menu.at.0.min(window.0 - w - 2 * s).max(2 * s);
    let top = menu.at.1.min(window.1 - height - 2 * s).max(2 * s);
    let mut y = top + PAD * s;
    let mut rows = Vec::new();
    for entry in &menu.entries {
        if entry.separator {
            y += SEPARATOR * s;
        }
        rows.push(Rect::new(left + s, y, left + w - s, y + row));
        y += row;
    }
    MenuLayout { panel: Rect::new(left, top, left + w, top + height), rows }
}

fn color(rgb: lazyterm::Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// Draw the menu.
pub fn paint(canvas: &mut dyn Canvas, menu: &Menu, laid: &MenuLayout, palette: &Palette, scale: i32) {
    let s = scale.max(1);
    let (bg, fg) = (palette.bg(), palette.fg());
    canvas.fill_rect(laid.panel, color(fg.lerp(bg, 0.6)));
    let inner = Rect::new(laid.panel.left + s, laid.panel.top + s, laid.panel.right - s, laid.panel.bottom - s);
    canvas.fill_rect(inner, color(bg.lerp(fg, 0.12)));
    let live = xui_core::TextStyle::new(color(fg), xui_core::Dip(15.0));
    let dead = xui_core::TextStyle::new(color(fg.lerp(bg, 0.55)), xui_core::Dip(15.0));
    let hint = xui_core::TextStyle::new(color(fg.lerp(bg, 0.5)), xui_core::Dip(13.0));
    for (i, (entry, row)) in menu.entries.iter().zip(&laid.rows).enumerate() {
        if entry.separator {
            let y = row.top - SEPARATOR * s / 2;
            canvas.fill_rect(Rect::new(row.left + 4 * s, y, row.right - 4 * s, y + s), color(fg.lerp(bg, 0.7)));
        }
        if menu.selected == Some(i) {
            canvas.fill_rect(*row, color(palette.cursor().lerp(bg, 0.55)));
        }
        let style = if entry.enabled { &live } else { &dead };
        let text_at = Rect::new(row.left + 10 * s, row.top + (row.height() - 20 * s) / 2, row.right, row.bottom);
        canvas.draw_text(entry.item.label(), text_at, style);
        let shortcut = entry.item.shortcut();
        if !shortcut.is_empty() {
            let at = Rect::new(row.right - 104 * s, row.top + (row.height() - 18 * s) / 2, row.right - 4 * s, row.bottom);
            canvas.draw_text(shortcut, at, &hint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu::new((100, 100), false, None)
    }

    #[test]
    fn copy_and_the_link_entries_follow_the_state() {
        let none = menu();
        assert!(!none.enabled(Item::Copy) && !none.enabled(Item::OpenLink) && !none.enabled(Item::CopyLink));
        assert!(none.enabled(Item::Paste) && none.enabled(Item::Find) && none.enabled(Item::Close));
        let with = Menu::new((0, 0), true, Some("http://x.y/".into()));
        assert!(with.enabled(Item::Copy) && with.enabled(Item::OpenLink) && with.enabled(Item::CopyLink));
    }

    #[test]
    fn the_keys_move_over_enabled_entries_and_choose() {
        let mut m = menu();
        assert_eq!(m.key(Key::RETURN), Step::None, "nothing selected yet");
        m.key(Key::DOWN);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::Paste, "Copy is disabled: skipped");
        m.key(Key::DOWN);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::SelectAll);
        m.key(Key::UP);
        m.key(Key::UP);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::Paste, "does not wrap past the first enabled");
        assert_eq!(m.key(Key::RETURN), Step::Choose(Item::Paste));
        m.key(Key::END);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::Close);
        m.key(Key::DOWN);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::Close, "stays on the last");
        m.key(Key::HOME);
        assert_eq!(m.entries[m.selected.unwrap()].item, Item::Paste);
        assert_eq!(m.key(Key::ESCAPE), Step::Close);
        assert_eq!(m.key(Key::from_code(0x41)), Step::None);
    }

    #[test]
    fn a_disabled_entry_is_never_chosen() {
        let mut m = menu();
        m.selected = Some(0);
        assert_eq!(m.key(Key::RETURN), Step::None);
        let laid = layout(&m, (640, 400), 1);
        let r = laid.rows[0];
        assert_eq!(m.click(&laid, r.left + 3, r.top + 3), Step::None);
        let paste = laid.rows[1];
        assert_eq!(m.click(&laid, paste.left + 3, paste.top + 3), Step::Choose(Item::Paste));
        assert_eq!(m.click(&laid, 1, 1), Step::Close, "outside closes");
    }

    #[test]
    fn the_panel_stays_inside_the_window() {
        let m = Menu::new((630, 395), true, None);
        for scale in [1, 2] {
            let window = (640 * scale, 400 * scale);
            let laid = layout(&m, window, scale);
            assert!(laid.panel.left >= 0 && laid.panel.top >= 0);
            assert!(laid.panel.right <= window.0 && laid.panel.bottom <= window.1, "{:?}", laid.panel);
            assert_eq!(laid.rows.len(), m.entries.len());
            assert!(laid.rows.iter().all(|r| laid.panel.left < r.left && r.right < laid.panel.right));
            // Rows do not overlap, in order.
            assert!(laid.rows.windows(2).all(|w| w[0].bottom <= w[1].top));
        }
        let near = layout(&Menu::new((50, 60), true, None), (640, 400), 1);
        assert_eq!((near.panel.left, near.panel.top), (50, 60));
    }

    #[test]
    fn hovering_selects_enabled_entries_only() {
        let mut m = menu();
        let laid = layout(&m, (640, 400), 1);
        m.hover(&laid, laid.rows[0].left + 2, laid.rows[0].top + 2);
        assert_eq!(m.selected, None, "Copy is disabled");
        m.hover(&laid, laid.rows[2].left + 2, laid.rows[2].top + 2);
        assert_eq!(m.selected, Some(2));
    }

    #[test]
    fn every_entry_has_a_probe_name_and_evidence() {
        let m = menu();
        let probes: Vec<String> = m.entries.iter().map(|e| e.item.probe()).collect();
        assert!(probes.contains(&"menu:copy".to_string()) && probes.contains(&"menu:preferences".to_string()));
        assert!(probes.iter().all(|p| !p.contains(' ')), "{probes:?}");
        let mut names: Vec<&str> = m.entries.iter().map(|e| e.item.evidence()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 12, "unique evidence names");
    }
}

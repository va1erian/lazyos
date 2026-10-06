//! The taskbar: which windows it lists, where everything sits on the bar, and
//! what a click on an entry does.
//!
//! The list is fed by the compositor's shell protocol: seeded from
//! `ListSurfaces` (a restarted shell rebuilds its bar from it), then kept up
//! to date by `SurfaceChanged` and `FocusChanged`. Desktop and panel surfaces
//! (the shell's own) never get an entry. Entries are kept in creation order,
//! which is surface-id order because the compositor numbers surfaces from 1
//! upwards.
//!
//! Geometry is panel-local: the bar is a `BAR_H`-tall panel at the bottom of
//! the screen, so a panel-local `y` plus `screen_h - BAR_H` is the screen `y`.

use crate::Rect;

/// Height of the taskbar panel.
pub const BAR_H: i32 = 32;
/// The "LazyOS" start button, panel-local: screen `(4, H-30, 80, 28)`.
pub const START_BUTTON: Rect = Rect::new(4, 2, 80, 28);
/// Where the first window entry starts.
pub const ENTRY_X: i32 = 92;
/// Widest an entry gets.
pub const ENTRY_MAX_W: i32 = 160;
/// Horizontal gap between entries.
pub const ENTRY_GAP: i32 = 4;
/// Entry top, panel-local (screen `H-29`).
pub const ENTRY_Y: i32 = 3;
/// Entry height.
pub const ENTRY_H: i32 = 26;
/// Narrowest entry still drawn; past that the remaining entries are hidden.
pub const ENTRY_MIN_W: i32 = 24;
/// Padding either side of the clock text.
pub const CLOCK_PAD: i32 = 12;

/// What a surface is, from the protocol's `Role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// An app window: listed.
    Window,
    /// The desktop or a panel (or an unknown role): never listed.
    Shell,
}

/// What a `SurfaceChanged` reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Destroyed,
    Minimized,
    Restored,
    Title,
    /// Moved, resized, (un)maximized, or a kind this shell does not know.
    Other,
}

/// One surface as `ListSurfaces` or `SurfaceChanged` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface<'a> {
    pub id: u64,
    pub role: Role,
    pub title: Option<&'a str>,
    pub minimized: bool,
    pub focused: bool,
}

/// One taskbar entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The compositor's surface id.
    pub surface: u64,
    pub title: String,
    pub minimized: bool,
}

/// How the visible list changed after an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// Nothing visible changed.
    None,
    /// A new entry (surface id, title).
    Added(u64, String),
    /// An entry went away.
    Removed(u64),
    /// An entry's title, minimized state or the focus changed.
    Changed,
}

/// What clicking an entry asks the compositor for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `ActivateSurface`: restore, raise and focus.
    Activate(u64),
    /// `MinimizeSurface`.
    Minimize(u64),
}

/// The window list and the focused window.
#[derive(Clone, Debug, Default)]
pub struct Taskbar {
    windows: Vec<Window>,
    focused: Option<u64>,
}

/// The title a window without one shows.
const UNTITLED: &str = "Window";

impl Taskbar {
    /// An empty bar.
    pub fn new() -> Taskbar {
        Taskbar::default()
    }

    /// The entries in bar order.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    /// The focused window, if the compositor reported one.
    pub fn focused(&self) -> Option<u64> {
        self.focused
    }

    /// Replace the list with a `ListSurfaces` snapshot; returns how many
    /// windows it holds.
    pub fn seed<'a>(&mut self, rows: impl IntoIterator<Item = Surface<'a>>) -> usize {
        self.windows.clear();
        self.focused = None;
        for row in rows {
            if row.role != Role::Window {
                continue;
            }
            if row.focused {
                self.focused = Some(row.id);
            }
            self.insert(row.id, row.title, row.minimized);
        }
        self.windows.len()
    }

    /// Apply one `SurfaceChanged`.
    ///
    /// The shell subscribes before it lists the surfaces, so events queued in
    /// between may repeat what the list already says: a repeated `Created`
    /// updates the entry, a `Destroyed` for an unknown id does nothing, and any
    /// other change for an unknown window adds it (so a missed `Created` never
    /// leaves a window without an entry).
    pub fn apply(&mut self, kind: ChangeKind, surface: &Surface<'_>) -> Delta {
        if surface.role != Role::Window {
            return Delta::None;
        }
        if kind == ChangeKind::Destroyed {
            return self.remove(surface.id);
        }
        let mut changed = false;
        if surface.focused && self.focused != Some(surface.id) {
            self.focused = Some(surface.id);
            changed = true;
        }
        let Some(index) = self.index(surface.id) else {
            let title = self.insert(surface.id, surface.title, surface.minimized);
            return Delta::Added(surface.id, title);
        };
        let window = &mut self.windows[index];
        let minimized = match kind {
            ChangeKind::Minimized => true,
            ChangeKind::Restored => false,
            _ => surface.minimized,
        };
        if window.minimized != minimized {
            window.minimized = minimized;
            changed = true;
        }
        if let Some(title) = surface.title.filter(|title| *title != window.title) {
            window.title = clean_title(Some(title));
            changed = true;
        }
        if changed {
            Delta::Changed
        } else {
            Delta::None
        }
    }

    /// Apply `FocusChanged`; `true` when the highlight moved.
    pub fn set_focus(&mut self, focused: Option<u64>) -> bool {
        let changed = self.focused != focused;
        self.focused = focused;
        changed
    }

    /// What a click on `surface`'s entry does: the focused, visible window is
    /// minimized; anything else (another window, or a minimized one) is
    /// activated. `None` when the bar has no such entry.
    pub fn click(&self, surface: u64) -> Option<Action> {
        let window = &self.windows[self.index(surface)?];
        if self.focused == Some(surface) && !window.minimized {
            Some(Action::Minimize(surface))
        } else {
            Some(Action::Activate(surface))
        }
    }

    /// Whether the bar lists `surface`.
    pub fn contains(&self, surface: u64) -> bool {
        self.index(surface).is_some()
    }

    fn index(&self, surface: u64) -> Option<usize> {
        self.windows
            .binary_search_by_key(&surface, |window| window.surface)
            .ok()
    }

    /// Insert in id order; returns the title used.
    fn insert(&mut self, surface: u64, title: Option<&str>, minimized: bool) -> String {
        let title = clean_title(title);
        let window = Window {
            surface,
            title: title.clone(),
            minimized,
        };
        match self
            .windows
            .binary_search_by_key(&surface, |window| window.surface)
        {
            Ok(at) => self.windows[at] = window,
            Err(at) => self.windows.insert(at, window),
        }
        title
    }

    fn remove(&mut self, surface: u64) -> Delta {
        let Some(index) = self.index(surface) else {
            return Delta::None;
        };
        self.windows.remove(index);
        if self.focused == Some(surface) {
            self.focused = None;
        }
        Delta::Removed(surface)
    }
}

/// A printable, bounded title (the compositor already sanitises titles; the
/// shell does not trust that).
fn clean_title(title: Option<&str>) -> String {
    let kept: String = title
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        String::from(UNTITLED)
    } else {
        String::from(kept)
    }
}

/// The clock's rectangle, panel-local, on a bar `screen_w` wide whose clock
/// text is at most `text_w` pixels wide.
pub fn clock_rect(screen_w: i32, text_w: i32) -> Rect {
    let width = (text_w + CLOCK_PAD * 2).clamp(0, screen_w.max(0));
    Rect::new(screen_w - width, 0, width, BAR_H)
}

/// The panel-local rectangle of each of `count` entries on a bar `screen_w`
/// wide whose right end reserves `right_reserved` pixels: the clock's width
/// plus the tray's ([`crate::tray::layout::Layout::reserved`]). Entries are
/// `min(160, equal share)` wide, `ENTRY_GAP` apart from `ENTRY_X`. An entry
/// that would be narrower than [`ENTRY_MIN_W`] or cross into the reserved
/// area is `None` (hidden).
pub fn entry_rects(count: usize, screen_w: i32, right_reserved: i32) -> Vec<Option<Rect>> {
    let right = screen_w - right_reserved - ENTRY_GAP;
    let avail = right - ENTRY_X;
    if count == 0 {
        return Vec::new();
    }
    let n = i32::try_from(count).unwrap_or(i32::MAX);
    let share = (avail - ENTRY_GAP.saturating_mul(n - 1)) / n;
    let width = share.clamp(ENTRY_MIN_W, ENTRY_MAX_W);
    (0..count)
        .map(|i| {
            let x = ENTRY_X + (i as i32) * (width + ENTRY_GAP);
            let rect = Rect::new(x, ENTRY_Y, width, ENTRY_H);
            (rect.x + rect.w <= right).then_some(rect)
        })
        .collect()
}

/// The entry index under panel-local `(x, y)` in `rects`.
pub fn entry_at(rects: &[Option<Rect>], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .position(|rect| rect.is_some_and(|rect| rect.contains(x, y)))
}

#[cfg(test)]
mod tests;

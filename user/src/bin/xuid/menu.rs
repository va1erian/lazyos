//! The desktop context menu (issue #323): a compositor-owned popup, like the
//! Alt+Tab overlay, opened by a right press on the bare desktop. Its entries
//! come from confd ([`menuitems`](super::menuitems)) followed by the apps the
//! package manager installed, and each launches an `init` registry app; the
//! last rows restart or shut down the machine ([`powermenu`](super::powermenu)).
//!
//! The compositor is one task, so the open state lives in relaxed atomics
//! (as `SHELL_DEAD` does) instead of being threaded through every `repaint`
//! call site; [`geometry`] and [`item_at`] are pure so they stay testable.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use user::messenger::display::{self, Canvas, Face, Rect};
use user::messenger::services::{self, INIT_NAME};
use user::sys;

use super::compositor::Compositor;
use super::menuitems;
use super::powermenu::{self, Outcome};
use super::theme::{overlay_bg, overlay_border, overlay_selected, overlay_text, TASKBAR_H};
use super::window::contains;

const ITEM_H: i32 = 20;
const PAD: i32 = 4;
const TEXT_PAD: i32 = 12;
const MIN_W: i32 = 140;
/// The longest the compositor waits on `init` (10 s at 100 Hz). `Launch`
/// replies after the spawn, which takes a moment under emulation; the
/// compositor pauses meanwhile, so this is a backstop, not the usual cost.
const LAUNCH_TIMEOUT_TICKS: u64 = 1000;

static OPEN: AtomicBool = AtomicBool::new(false);
static ORIGIN_X: AtomicI32 = AtomicI32::new(0);
static ORIGIN_Y: AtomicI32 = AtomicI32::new(0);
/// Hovered item index, or `-1`.
static HOVER: AtomicI32 = AtomicI32::new(-1);

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// The menu rectangle for a press at `at` on a `screen` of the given size,
/// for `count` rows whose widest label is `label_w` wide: anchored at the
/// pointer, shifted to stay on screen and above the taskbar.
fn geometry(at: (i32, i32), screen: (i32, i32), count: usize, label_w: i32) -> Rect {
    let w = (label_w + TEXT_PAD * 2).max(MIN_W);
    let h = count as i32 * ITEM_H + PAD * 2;
    Rect::new(
        at.0.min(screen.0 - w).max(0),
        at.1.min(screen.1 - TASKBAR_H - h).max(0),
        w,
        h,
    )
}

/// The item index under `point` for a menu of `count` rows occupying `menu`.
fn item_at(menu: Rect, point: (i32, i32), count: usize) -> Option<usize> {
    let inside = Rect::new(menu.x, menu.y + PAD, menu.w, count as i32 * ITEM_H);
    contains(inside, point).then(|| ((point.1 - inside.y) / ITEM_H) as usize)
}

/// The open menu's rectangle (damage for repaints).
pub(super) fn rect(screen: (i32, i32)) -> Rect {
    let at = (
        ORIGIN_X.load(Ordering::Relaxed),
        ORIGIN_Y.load(Ordering::Relaxed),
    );
    menuitems::with(|items| {
        let label_w = items
            .iter()
            .map(|entry| Face::Sans.width(&entry.label))
            .max()
            .unwrap_or(0);
        geometry(at, screen, items.len(), label_w)
    })
}

/// Open the menu at `at`; returns the damage to repaint.
pub(super) fn open(at: (i32, i32), screen: (i32, i32)) -> Rect {
    ORIGIN_X.store(at.0, Ordering::Relaxed);
    ORIGIN_Y.store(at.1, Ordering::Relaxed);
    HOVER.store(-1, Ordering::Relaxed);
    // The installed apps are part of the menu: one `ListApps` as it opens, so
    // an app installed or removed a moment ago is already (or no longer) there.
    menuitems::refresh_installed();
    OPEN.store(true, Ordering::Relaxed);
    sys::write_str("XUID:MENU:OPEN\n");
    rect(screen)
}

/// Close the menu (dropping a pending confirmation); returns the damage to
/// repaint (empty if it was closed).
pub(super) fn close(screen: (i32, i32)) -> Rect {
    if !OPEN.swap(false, Ordering::Relaxed) {
        return Rect::new(0, 0, 0, 0);
    }
    let damage = rect(screen);
    menuitems::clear_confirm();
    damage
}

/// Track the pointer; returns the damage when the highlight changed.
pub(super) fn hover(point: (i32, i32), screen: (i32, i32)) -> Rect {
    let menu = rect(screen);
    let count = menuitems::with(|items| items.len());
    let now = item_at(menu, point, count).map_or(-1, |index| index as i32);
    if HOVER.swap(now, Ordering::Relaxed) == now {
        return Rect::new(0, 0, 0, 0);
    }
    menu
}

/// Whether `point` is on the open menu (its frame or an item).
pub(super) fn hit(point: (i32, i32), screen: (i32, i32)) -> bool {
    is_open() && contains(rect(screen), point)
}

/// Activate the item under `point`, if any: launch it, or act on a power row.
/// Returns what the menu does next.
pub(super) fn activate(point: (i32, i32), screen: (i32, i32)) -> Outcome {
    let count = menuitems::with(|items| items.len());
    let Some(index) = item_at(rect(screen), point, count) else {
        return Outcome::Close;
    };
    let Some(app) = menuitems::with(|items| items.get(index).map(|entry| entry.app.clone())) else {
        return Outcome::Close;
    };
    let app = app.as_str();
    if powermenu::is_power_row(app) {
        return powermenu::activate(app);
    }
    let deadline = Some(sys::clock() + LAUNCH_TIMEOUT_TICKS);
    let result = services::resolve_service(INIT_NAME)
        .and_then(|init| services::launch_by(&init, app, "", 0, deadline));
    match result {
        Ok(_) => {
            sys::write_str(&alloc::format!(
                "XUID:MENU:LAUNCH:{app}
"
            ));
        }
        Err(error) => {
            let code = error.errno().unwrap_or(0);
            sys::write_str(&alloc::format!(
                "XUID:MENU:LAUNCH:FAIL:{app}:{code}
"
            ));
        }
    }
    Outcome::Close
}

/// Paint the menu over `clip` (no-op when closed).
pub(super) fn draw(screen: &mut Canvas, clip: Rect) {
    if !is_open() {
        return;
    }
    let menu = rect((screen.width(), screen.height()));
    if menu.intersect(clip).is_empty() {
        return;
    }
    screen.fill(menu, clip, overlay_border());
    screen.fill(
        Rect::new(menu.x + 1, menu.y + 1, menu.w - 2, menu.h - 2),
        clip,
        overlay_bg(),
    );
    let hovered = HOVER.load(Ordering::Relaxed);
    menuitems::with(|items| {
        for (index, entry) in items.iter().enumerate() {
            let label = entry.label.as_str();
            let row = Rect::new(
                menu.x + 2,
                menu.y + PAD + index as i32 * ITEM_H,
                menu.w - 4,
                ITEM_H,
            );
            if index as i32 == hovered {
                screen.fill(row, clip, overlay_selected());
            }
            screen.text_face(
                row.x + TEXT_PAD - 2,
                row.y + (ITEM_H - Face::Sans.height()) / 2,
                label,
                Face::Sans,
                overlay_text(),
                row.intersect(clip),
            );
        }
    });
}

impl Compositor {
    /// Open the menu at a right press on the bare desktop (`at` is the pointer)
    /// and paint it.
    pub(super) fn menu_open_at(&mut self, at: (i32, i32)) {
        let damage = open(at, (self.screen.width(), self.screen.height()));
        self.repaint(damage);
    }

    /// A pointer move while the menu is open: move the cursor, update the
    /// highlight. The caller has already stored the new pointer position.
    pub(super) fn menu_pointer_moved(&mut self, old: (i32, i32)) {
        let new = self.pointer;
        let damage = super::layout::cursor_rect(old)
            .union(super::layout::cursor_rect(new))
            .union(hover(new, (self.screen.width(), self.screen.height())));
        self.repaint(damage);
    }

    /// A button press while the menu may be open. A left press on an item
    /// launches it; any press dismisses the menu. Returns `true` when the
    /// press landed on the menu (consumed); a press elsewhere carries on
    /// normally.
    pub(super) fn menu_press(&mut self, point: (i32, i32), button: u32) -> bool {
        if !is_open() {
            return false;
        }
        let dims = (self.screen.width(), self.screen.height());
        let on_menu = hit(point, dims);
        let before = rect(dims);
        let outcome = if on_menu && button == display::button::LEFT {
            activate(point, dims)
        } else {
            Outcome::Close
        };
        match outcome {
            Outcome::KeepOpen => {
                // The rows changed (a confirmation): repaint old and new.
                HOVER.store(-1, Ordering::Relaxed);
                self.repaint(before.union(rect(dims)));
            }
            Outcome::Close => {
                let damage = close(dims);
                self.repaint(damage);
            }
            Outcome::CloseAndRepaint => {
                close(dims);
                self.repaint_full();
            }
        }
        on_menu
    }

    /// Escape closes an open menu; returns whether it did.
    pub(super) fn menu_escape(&mut self) -> bool {
        if !is_open() {
            return false;
        }
        let damage = close((self.screen.width(), self.screen.height()));
        self.repaint(damage);
        true
    }
}

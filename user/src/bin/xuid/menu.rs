//! The desktop context menu (issue #323): a compositor-owned popup, like the
//! Alt+Tab overlay, opened by a right press on the bare desktop. Its entries
//! are hardcoded for now and each launches an `init` registry app.
//!
//! The compositor is one task, so the open state lives in relaxed atomics
//! (as `SHELL_DEAD` does) instead of being threaded through every `repaint`
//! call site; [`geometry`] and [`item_at`] are pure so they stay testable.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use user::messenger::display::{self, Canvas, Rect};
use user::messenger::services::{self, INIT_NAME};
use user::sys;

use super::render::repaint;
use super::shell::AltTab;
use super::surface::Surface;
use super::theme::{OVERLAY_BG, OVERLAY_BORDER, OVERLAY_SELECTED, OVERLAY_TEXT, TASKBAR_H};
use super::window::contains;

/// The hardcoded entries: `(init app id, label)`. Terminal first.
const ITEMS: &[(&str, &str)] = &[
    ("terminal", "Terminal"),
    ("sysmon", "System Monitor"),
    ("fabricmon", "Fabric Monitor"),
    ("counter", "Counter"),
];
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

/// The menu rectangle for a press at `at` on a `screen` of the given size:
/// anchored at the pointer, shifted to stay on screen and above the taskbar.
fn geometry(at: (i32, i32), screen: (i32, i32)) -> Rect {
    let label_w = ITEMS
        .iter()
        .map(|(_, label)| label.len() as i32 * display::font::ADVANCE)
        .max()
        .unwrap_or(0);
    let w = (label_w + TEXT_PAD * 2).max(MIN_W);
    let h = ITEMS.len() as i32 * ITEM_H + PAD * 2;
    Rect::new(
        at.0.min(screen.0 - w).max(0),
        at.1.min(screen.1 - TASKBAR_H - h).max(0),
        w,
        h,
    )
}

/// The item index under `point` for a menu occupying `menu`.
fn item_at(menu: Rect, point: (i32, i32)) -> Option<usize> {
    let inside = Rect::new(menu.x, menu.y + PAD, menu.w, ITEMS.len() as i32 * ITEM_H);
    contains(inside, point).then(|| ((point.1 - inside.y) / ITEM_H) as usize)
}

/// The open menu's rectangle (damage for repaints).
pub(super) fn rect(screen: (i32, i32)) -> Rect {
    geometry(
        (
            ORIGIN_X.load(Ordering::Relaxed),
            ORIGIN_Y.load(Ordering::Relaxed),
        ),
        screen,
    )
}

/// Open the menu at `at`; returns the damage to repaint.
pub(super) fn open(at: (i32, i32), screen: (i32, i32)) -> Rect {
    ORIGIN_X.store(at.0, Ordering::Relaxed);
    ORIGIN_Y.store(at.1, Ordering::Relaxed);
    HOVER.store(-1, Ordering::Relaxed);
    OPEN.store(true, Ordering::Relaxed);
    sys::write_str("XUID:MENU:OPEN\n");
    rect(screen)
}

/// Close the menu; returns the damage to repaint (empty if it was closed).
pub(super) fn close(screen: (i32, i32)) -> Rect {
    if !OPEN.swap(false, Ordering::Relaxed) {
        return Rect::new(0, 0, 0, 0);
    }
    rect(screen)
}

/// Track the pointer; returns the damage when the highlight changed.
pub(super) fn hover(point: (i32, i32), screen: (i32, i32)) -> Rect {
    let menu = rect(screen);
    let now = item_at(menu, point).map_or(-1, |index| index as i32);
    if HOVER.swap(now, Ordering::Relaxed) == now {
        return Rect::new(0, 0, 0, 0);
    }
    menu
}

/// Whether `point` is on the open menu (its frame or an item).
pub(super) fn hit(point: (i32, i32), screen: (i32, i32)) -> bool {
    is_open() && contains(rect(screen), point)
}

/// Activate the item under `point`, if any: launch it. The caller closes the
/// menu either way.
pub(super) fn activate(point: (i32, i32), screen: (i32, i32)) {
    let Some(index) = item_at(rect(screen), point) else {
        return;
    };
    let (app, _) = ITEMS[index];
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
    screen.fill(menu, clip, OVERLAY_BORDER);
    screen.fill(
        Rect::new(menu.x + 1, menu.y + 1, menu.w - 2, menu.h - 2),
        clip,
        OVERLAY_BG,
    );
    let hovered = HOVER.load(Ordering::Relaxed);
    for (index, (_, label)) in ITEMS.iter().enumerate() {
        let row = Rect::new(
            menu.x + 2,
            menu.y + PAD + index as i32 * ITEM_H,
            menu.w - 4,
            ITEM_H,
        );
        if index as i32 == hovered {
            screen.fill(row, clip, OVERLAY_SELECTED);
        }
        screen.text(
            row.x + TEXT_PAD - 2,
            row.y + (ITEM_H - display::font::H) / 2,
            label,
            OVERLAY_TEXT,
            row.intersect(clip),
            1,
        );
    }
}

/// Repaint `damage` with the ambient compositor state (no drag session).
fn refresh(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    taskbar: bool,
    alt_tab: &Option<AltTab>,
) {
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        None,
        taskbar,
        alt_tab.as_ref(),
    );
}

/// Open the menu at a right press on the bare desktop and paint it.
pub(super) fn open_at(
    screen: &mut Canvas,
    surfaces: &[Surface],
    at: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    alt_tab: &Option<AltTab>,
) {
    let damage = open(at, (screen.width(), screen.height()));
    refresh(screen, surfaces, at, focused, damage, taskbar, alt_tab);
}

/// A pointer move while the menu is open: move the cursor, update the highlight.
pub(super) fn pointer_moved(
    screen: &mut Canvas,
    surfaces: &[Surface],
    old: (i32, i32),
    new: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    alt_tab: &Option<AltTab>,
) {
    let damage = super::layout::cursor_rect(old)
        .union(super::layout::cursor_rect(new))
        .union(hover(new, (screen.width(), screen.height())));
    refresh(screen, surfaces, new, focused, damage, taskbar, alt_tab);
}

/// A button press while the menu may be open. A left press on an item
/// launches it; any press dismisses the menu. Returns `true` when the press
/// landed on the menu (consumed); a press elsewhere carries on normally.
pub(super) fn press(
    screen: &mut Canvas,
    surfaces: &[Surface],
    point: (i32, i32),
    button: u32,
    focused: Option<u64>,
    taskbar: bool,
    alt_tab: &Option<AltTab>,
) -> bool {
    if !is_open() {
        return false;
    }
    let dims = (screen.width(), screen.height());
    let on_menu = hit(point, dims);
    if on_menu && button == display::button::LEFT {
        activate(point, dims);
    }
    let damage = close(dims);
    refresh(screen, surfaces, point, focused, damage, taskbar, alt_tab);
    on_menu
}

/// Escape closes an open menu; returns whether it did.
pub(super) fn escape(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    taskbar: bool,
    alt_tab: &Option<AltTab>,
) -> bool {
    if !is_open() {
        return false;
    }
    let damage = close((screen.width(), screen.height()));
    refresh(screen, surfaces, pointer, focused, damage, taskbar, alt_tab);
    true
}

//! Window affordances (issue #161, docs/shell-plan.md S5.3): snapping a
//! window to half of the work area, the Super+arrow shortcuts, snapping by
//! dragging a title bar to a screen edge, and the `XUID:WM:*` serial markers
//! every window-manager action prints, so a session can wait for one.
//!
//! * **Super+Left / Super+Right** snap the focused window to the left or
//!   right half of the work area (from normal, maximized or the other half).
//! * **Super+Up** maximizes it; **Super+Down** restores a maximized or
//!   snapped window to its saved normal rectangle, and minimizes a normal one.
//! * Releasing a title-bar drag with the pointer on the left or right screen
//!   edge snaps the window there; on the top edge it maximizes. Dragging a
//!   snapped window off keeps its size and forgets the snap.
//!
//! A snapped window keeps its normal rectangle (`Surface::snap`) like a
//! maximized one does, is re-fitted when the work area changes, and its
//! client hears a `Configure` (state `Normal`) with the new size. Only a
//! resizable, visible window snaps.
//!
//! Serial: `XUID:WM:SNAP side=<left|right>`, `XUID:WM:MAXIMIZE`,
//! `XUID:WM:RESTORE`, `XUID:WM:MINIMIZE`, `XUID:WM:UNMINIMIZE`,
//! `XUID:WM:OPENED`, `XUID:WM:CLOSED` (the windows left), and for Alt+Tab
//! `XUID:WM:ALTTAB n=<entries>` (the selected entry) and
//! `XUID:WM:ALTTAB:COMMIT`, each followed by `id=<n> windows=<n>
//! title=<title>`; `XUID:SNAP:PASS|FAIL` is the boot check of the pure rules.

use alloc::format;
use alloc::vec::Vec;
use user::messenger::display::{self, wire, Rect};
use user::sys;

use super::compositor::Compositor;

/// Which half of the work area a snapped window fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Left,
    Right,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

/// What releasing a title-bar drag at the pointer does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeDrop {
    Snap(Side),
    Maximize,
}

/// How close to a screen edge (pixels) a drag must end to snap.
const EDGE: i32 = 2;

/// The decorated window rectangle of a window snapped to `side` of `work`:
/// one half each, the right half taking the odd pixel.
pub(super) fn snapped_rect(work: Rect, side: Side) -> Rect {
    let left = work.w / 2;
    match side {
        Side::Left => Rect::new(work.x, work.y, left, work.h),
        Side::Right => Rect::new(work.x + left, work.y, work.w - left, work.h),
    }
}

/// What a title-bar drag released at `pointer` on `screen` asks for.
pub(super) fn edge_drop(pointer: (i32, i32), screen: Rect) -> Option<EdgeDrop> {
    let (x, y) = pointer;
    if x < screen.x + EDGE {
        Some(EdgeDrop::Snap(Side::Left))
    } else if x >= screen.x + screen.w - EDGE {
        Some(EdgeDrop::Snap(Side::Right))
    } else if y < screen.y + EDGE {
        Some(EdgeDrop::Maximize)
    } else {
        None
    }
}

impl Compositor {
    /// Print `XUID:WM:<what>` for surface `id`.
    pub(super) fn wm_mark(&self, what: &str, id: u64) {
        // A closing window is still in the table: count what stays.
        let closing = what == "CLOSED";
        let windows = self
            .surfaces
            .iter()
            .filter(|s| s.is_window() && !(closing && s.id == id))
            .count();
        let title = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)
            .map_or("", |surface| surface.title.as_str());
        sys::write_str(&format!(
            "XUID:WM:{what} id={id} windows={windows} title={title}\n"
        ));
    }

    /// A Super+arrow shortcut on the focused window; whether `key` was one.
    pub(super) fn wm_key(&mut self, key: u32) -> bool {
        let action = match key {
            display::key::LEFT => Some(Side::Left),
            display::key::RIGHT => Some(Side::Right),
            display::key::UP | display::key::DOWN => None,
            _ => return false,
        };
        let Some(id) = self.focused else {
            return true;
        };
        match action {
            Some(side) => self.snap(id, side),
            None if key == display::key::UP => {
                self.unsnap_in_place(id);
                self.maximize(id);
            }
            None => {
                let state = self.surfaces.iter().find(|surface| surface.id == id);
                let (maximized, snapped) = state.map_or((false, false), |s| {
                    (s.maximized.is_some(), s.snap.is_some())
                });
                if maximized {
                    self.unmaximize(id);
                } else if snapped {
                    self.unsnap(id);
                } else {
                    self.minimize_surface(id);
                }
            }
        }
        true
    }

    /// Snap window `id` to `side` of the work area, keeping its normal
    /// rectangle to come back to.
    pub(super) fn snap(&mut self, id: u64, side: Side) {
        self.finish_opening();
        let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) else {
            return;
        };
        if !surface.resizable() || surface.minimized {
            return;
        }
        let from = surface.window();
        let normal = surface
            .snap
            .map(|(_, rect)| rect)
            .or(surface.maximized)
            .unwrap_or(from);
        let was_maximized = surface.maximized.take().is_some();
        surface.snap = Some((side, normal));
        let target = snapped_rect(self.work_area(), side);
        self.set_minimized(id, true);
        self.zoom_two_step(id, from, target);
        self.apply_window(id, target);
        self.set_minimized(id, false);
        let change = if was_maximized {
            wire::CHANGE_UNMAXIMIZED
        } else {
            wire::CHANGE_RESIZED
        };
        self.configure_and_notify(id, wire::WINDOW_STATE_NORMAL, change);
        self.wm_mark(&format!("SNAP side={}", side.label()), id);
        self.repaint_full();
    }

    /// Restore snapped window `id` to its normal rectangle, animated.
    pub(super) fn unsnap(&mut self, id: u64) {
        self.finish_opening();
        let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) else {
            return;
        };
        let Some((_, normal)) = surface.snap.take() else {
            return;
        };
        let from = surface.window();
        self.set_minimized(id, true);
        self.zoom_two_step(id, from, normal);
        self.apply_window(id, normal);
        self.set_minimized(id, false);
        self.configure_and_notify(id, wire::WINDOW_STATE_NORMAL, wire::CHANGE_RESIZED);
        self.wm_mark("RESTORE", id);
        self.repaint_full();
    }

    /// Put snapped window `id` back on its normal rectangle at once, before
    /// a maximize saves that rectangle as the one to restore.
    fn unsnap_in_place(&mut self, id: u64) {
        let normal = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.id == id)
            .and_then(|surface| surface.snap.take())
            .map(|(_, rect)| rect);
        if let Some(normal) = normal {
            self.apply_window(id, normal);
        }
    }

    /// A title-bar drag of `id` began: a snapped window leaves its half
    /// (keeping its size).
    pub(super) fn drag_began(&mut self, id: u64) {
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.snap = None;
        }
    }

    /// A title-bar drag of `id` ended at the pointer: snap or maximize at a
    /// screen edge.
    pub(super) fn drag_ended(&mut self, id: u64) {
        match edge_drop(self.pointer, self.full()) {
            Some(EdgeDrop::Snap(side)) => self.snap(id, side),
            Some(EdgeDrop::Maximize) => self.maximize(id),
            None => {}
        }
    }

    /// Re-fit every snapped window to the current work area.
    pub(super) fn reflow_snapped(&mut self) {
        let work = self.work_area();
        let snapped: Vec<(u64, Side)> = self
            .surfaces
            .iter()
            .filter_map(|surface| surface.snap.map(|(side, _)| (surface.id, side)))
            .collect();
        for (id, side) in snapped {
            let target = snapped_rect(work, side);
            let current = self
                .surfaces
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.window());
            if current != Some(target) {
                self.apply_window(id, target);
                self.configure_and_notify(id, wire::WINDOW_STATE_NORMAL, wire::CHANGE_RESIZED);
            }
        }
    }
}

/// Boot check of the pure rules: `XUID:SNAP:PASS` or `XUID:SNAP:FAIL`.
pub(super) fn selftest_wm() -> &'static str {
    let work = Rect::new(0, 0, 1281, 688);
    let left = snapped_rect(work, Side::Left);
    let right = snapped_rect(work, Side::Right);
    let halves = left == Rect::new(0, 0, 640, 688)
        && right == Rect::new(640, 0, 641, 688)
        && left.x + left.w == right.x;
    let offset = snapped_rect(Rect::new(10, 20, 100, 50), Side::Right) == Rect::new(60, 20, 50, 50);
    let screen = Rect::new(0, 0, 1280, 720);
    let edges = edge_drop((0, 300), screen) == Some(EdgeDrop::Snap(Side::Left))
        && edge_drop((1279, 300), screen) == Some(EdgeDrop::Snap(Side::Right))
        && edge_drop((600, 0), screen) == Some(EdgeDrop::Maximize)
        && edge_drop((0, 0), screen) == Some(EdgeDrop::Snap(Side::Left))
        && edge_drop((600, 300), screen).is_none()
        && edge_drop((2, 300), screen).is_none();
    if halves && offset && edges {
        "XUID:SNAP:PASS\n"
    } else {
        "XUID:SNAP:FAIL\n"
    }
}

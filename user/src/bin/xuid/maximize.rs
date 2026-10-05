//! Maximize and restore: toggling a resizable window between its
//! saved normal rectangle and the work area, using the same wireframe zoom
//! animation as minimize, and re-fitting maximized windows when the work area
//! changes (a shell subscribes or dies).

use alloc::vec::Vec;
use user::messenger::display::{wire, Rect};

use super::compositor::Compositor;
use super::geometry;
use super::theme::{border, title_h};

impl Compositor {
    /// Maximize surface `id` if it is normal, restore it if it is maximized.
    pub(super) fn toggle_maximize(&mut self, id: u64) {
        let maximized = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)
            .is_some_and(|surface| surface.maximized.is_some());
        if maximized {
            self.unmaximize(id);
        } else {
            self.maximize(id);
        }
    }

    /// Maximize surface `id` to the work area with the zoom animation.
    pub(super) fn maximize(&mut self, id: u64) {
        // A window still opening lands first: it is hidden while it flies
        // in, which these state changes would misread as minimized.
        self.finish_opening();
        let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) else {
            return;
        };
        // Only a resizable, visible, currently-normal window can maximize.
        if !surface.resizable() || surface.minimized || surface.maximized.is_some() {
            return;
        }
        let from = surface.window();
        let target = geometry::maximized_rect(self.work_area());
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.maximized = Some(from);
        }
        // Hide the window for the animation, like `iconify`.
        self.set_minimized(id, true);
        self.zoom_two_step(id, from, target);
        self.apply_window(id, target);
        self.set_minimized(id, false);
        self.configure_and_notify(id, wire::WINDOW_STATE_MAXIMIZED, wire::CHANGE_MAXIMIZED);
        self.repaint_full();
    }

    /// Restore surface `id` to the rectangle saved when it was maximized.
    pub(super) fn unmaximize(&mut self, id: u64) {
        // A window still opening lands first: it is hidden while it flies
        // in, which these state changes would misread as minimized.
        self.finish_opening();
        let Some(restore) = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)
            .and_then(|surface| surface.maximized)
        else {
            return;
        };
        let from = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)
            .map_or(restore, |surface| surface.window());
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.maximized = None;
        }
        self.set_minimized(id, true);
        self.zoom_two_step(id, from, restore);
        self.apply_window(id, restore);
        self.set_minimized(id, false);
        self.configure_and_notify(id, wire::WINDOW_STATE_NORMAL, wire::CHANGE_UNMAXIMIZED);
        self.repaint_full();
    }

    /// Re-fit every maximized window to the current work area and tell its
    /// client. Called when a shell subscribes or its endpoint dies, which
    /// changes whether the fallback taskbar occupies part of the screen.
    pub(super) fn reflow_maximized(&mut self) {
        let target = geometry::maximized_rect(self.work_area());
        let ids: Vec<u64> = self
            .surfaces
            .iter()
            .filter(|surface| surface.maximized.is_some())
            .map(|surface| surface.id)
            .collect();
        for id in ids {
            let current = self
                .surfaces
                .iter()
                .find(|surface| surface.id == id)
                .map(|surface| surface.window());
            if current != Some(target) {
                self.apply_window(id, target);
                self.configure_and_notify(id, wire::WINDOW_STATE_MAXIMIZED, wire::CHANGE_MAXIMIZED);
            }
        }
    }

    /// Move and size surface `id` to the decorated window rectangle `rect`.
    fn apply_window(&mut self, id: u64, rect: Rect) {
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.x = rect.x;
            surface.y = rect.y;
            surface.w = rect.w - border() * 2;
            surface.h = rect.h - title_h() - border();
        }
    }

    /// Send the surface's client a `Configure` with its current content size
    /// and `state`, then tell the shell `change`.
    fn configure_and_notify(&mut self, id: u64, state: u32, change: u32) {
        if let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) {
            let (events, width, height) = (surface.events, surface.w, surface.h);
            self.send_configure(id, events, width, height, state);
        }
        self.notify_surface(id, change);
    }
}

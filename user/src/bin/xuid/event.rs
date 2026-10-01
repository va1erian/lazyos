//! Input routing (issue #194 split): [`Compositor::handle_event`] turns kernel
//! pointer events into window-management actions, drag & drop routing, focus
//! changes and forwarded client events. Key events live in `keys.rs`.

use user::messenger::display::{self, wire};
use user::sys;

use super::compositor::Compositor;
use super::geometry;
use super::layout::{cursor_rect, taskbar_hit};
use super::protocol::{Event, EventKind};
use super::surface::Drag;
use super::theme::{DOUBLE_CLICK_SLOP, DOUBLE_CLICK_TICKS, RESIZE_OUT, TASKBAR_H};
use super::window::{contains, forward, raise, relative, surface_by_id};

impl Compositor {
    /// Route one input event: window-management actions first (taskbar,
    /// title-bar buttons, drag, raise), then focus/cursor updates, then
    /// forward the event to the focused surface's event endpoint. While a drag
    /// & drop session is live (issue #145) the compositor owns the pointer and
    /// routes it through the drag section instead.
    pub(super) fn handle_event(&mut self, event: Event) {
        // The machine is going down: nothing under the overlay takes input.
        if super::powerfeed::active() {
            return;
        }
        match event.kind {
            EventKind::PointerMove => self.pointer_move((event.a as i32, event.b as i32)),
            EventKind::PointerDown => self.pointer_down(event.a as u32),
            EventKind::PointerUp => self.pointer_up(event.a as u32),
            EventKind::KeyDown => self.key_down(event.a as u32),
            EventKind::KeyUp => self.key_up(event.a as u32),
            EventKind::PointerWheel => self.pointer_wheel(event.a as i32),
        }
    }

    /// The pointer moved to `new`.
    fn pointer_move(&mut self, new: (i32, i32)) {
        let old = self.pointer;
        if super::menu::is_open() && self.drag_session.is_none() {
            self.pointer = new;
            self.menu_pointer_moved(old);
            return;
        }
        // A drag & drop session owns the pointer (issue #145): the surface
        // under it gets enter/over/leave, and the source hears nothing until
        // the session ends.
        if self.drag_session.is_some() {
            self.pointer = new;
            let damage = self.drag_move(old);
            self.repaint(damage);
            return;
        }
        // An interactive resize owns the pointer: the wireframe follows it and
        // the window itself does not change until release.
        if self.resize.is_some() {
            self.pointer = new;
            self.resize_move(old, new);
            return;
        }
        let mut damage = cursor_rect(old).union(cursor_rect(new));
        self.pointer = new;
        if let Some(active) = self.drag {
            // A title-bar drag: place the window so the grabbed point stays
            // under the pointer (exact even if events were coalesced),
            // clamped to the screen and the space above the taskbar.
            damage = damage.union(self.move_dragged_window(active, new));
            // The matching press was consumed by the title bar, so the moves
            // stay in the compositor: the app never saw the grab.
            self.repaint(damage);
            return;
        }
        // Moves are surface-relative like presses (issue #287); they go to the
        // focused surface and may fall outside it mid-drag.
        if let Some(id) = self.focused {
            let (x, y) = relative(&self.surfaces, id, new);
            let body = wire::encode_pointer_move_args(&wire::PointerMoveArgs { x, y });
            forward(
                &self.surfaces,
                &mut self.scratch,
                Some(id),
                wire::METHOD_POINTERMOVE,
                body,
            );
        }
        self.repaint(damage);
    }

    /// Move the title-bar-dragged window under `pointer`; returns the damage
    /// (old and new window rectangles), empty when the window is gone. The
    /// window may be pushed partly off the left, right and bottom edges, but
    /// its title bar stays reachable.
    fn move_dragged_window(&mut self, active: Drag, pointer: (i32, i32)) -> display::Rect {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.id == active.id && !surface.minimized)
        else {
            return display::Rect::new(0, 0, 0, 0);
        };
        let window = self.surfaces[index].window();
        let target = display::Rect::new(
            pointer.0 - active.grab_x,
            pointer.1 - active.grab_y,
            window.w,
            window.h,
        );
        let (x, y) = geometry::keep_reachable(target, self.work_area());
        let surface = &mut self.surfaces[index];
        surface.x = x;
        surface.y = y;
        window.union(surface.window())
    }

    /// A pointer button went down.
    fn pointer_down(&mut self, button: u32) {
        // `consumed` bit for the button in a press/release event.
        let button_bit = 1u32 << (button & 31);
        let (screen_w, screen_h) = (self.screen.width(), self.screen.height());
        let bar = self.taskbar();
        self.button_down = true;
        if self.drag_session.is_some() {
            // A second press while a drag & drop session is live is ignored;
            // the session ends on the first release.
            return;
        }
        let point = self.pointer;
        if self.menu_press(point, button) {
            self.consumed |= button_bit;
            return;
        }
        // The fallback taskbar paints above every window, so it hit-tests
        // first; with a shell registered it is hidden and not hit-tested.
        if bar {
            if let Some(id) = taskbar_hit(&self.surfaces, screen_w, screen_h, point) {
                self.consumed |= button_bit;
                self.restore_and_focus(id);
                self.repaint_full();
                return;
            }
        }
        // A press outside every window is a desktop click: ignore it. The
        // window rectangle is inflated by the outer half of the resize grip so
        // a press just outside a resizable frame still grabs its edge.
        let Some(index) = self.surfaces.iter().rposition(|surface| {
            !surface.desktop
                && !surface.minimized
                && contains(geometry::inflate(surface.window(), RESIZE_OUT), point)
        }) else {
            self.consumed |= button_bit;
            let over_bar = bar && point.1 >= screen_h - TASKBAR_H;
            if button == display::button::RIGHT && !over_bar {
                self.menu_open_at(point);
            }
            return;
        };
        let (id, origin, close, minimize, maximize, title, window, resizable, maximized) = {
            let surface = &self.surfaces[index];
            (
                surface.id,
                (surface.x, surface.y),
                surface.close_button(),
                surface.minimize_button(),
                surface.maximize_button(),
                surface.title_bar(),
                surface.window(),
                surface.resizable(),
                surface.maximized.is_some(),
            )
        };
        let before = self.focused;
        raise(&mut self.surfaces, id);
        self.focused = Some(id);
        if self.focused != before {
            self.notify_focus();
        }
        let left = button == display::button::LEFT;
        if left && contains(close, point) {
            self.consumed |= button_bit;
            self.close_surface(id);
            return;
        }
        if left && contains(minimize, point) {
            self.consumed |= button_bit;
            self.minimize_surface(id);
            return;
        }
        if left && resizable && contains(maximize, point) {
            self.consumed |= button_bit;
            self.toggle_maximize(id);
            return;
        }
        // An interactive resize, before the title-bar move handle. It never
        // starts during a drag & drop session, an open menu, Alt+Tab or an
        // existing drag, and never on a maximized window.
        if left
            && resizable
            && !maximized
            && self.resize.is_none()
            && self.drag.is_none()
            && self.alt_tab.is_none()
        {
            let edges = geometry::hit_edges(window, point);
            if !edges.is_empty() {
                self.consumed |= button_bit;
                self.begin_resize(id, edges, point);
                return;
            }
        }
        if contains(title, point) {
            self.consumed |= button_bit;
            if left {
                // Two presses on the same title bar in quick succession
                // maximize or restore instead of starting a drag.
                let now = sys::clock();
                let double = self.last_title_click.is_some_and(|(click_id, tick, at)| {
                    click_id == id
                        && now.saturating_sub(tick) <= DOUBLE_CLICK_TICKS
                        && (point.0 - at.0).abs() <= DOUBLE_CLICK_SLOP
                        && (point.1 - at.1).abs() <= DOUBLE_CLICK_SLOP
                });
                if double && resizable {
                    self.last_title_click = None;
                    self.toggle_maximize(id);
                    return;
                }
                self.last_title_click = Some((id, now, point));
                // A maximized window does not move on a title-bar drag; only
                // the double-click above restores it.
                if self.alt_tab.is_none() && !maximized {
                    self.drag = Some(Drag {
                        id,
                        grab_x: point.0 - origin.0,
                        grab_y: point.1 - origin.1,
                    });
                }
            }
            // Title-bar presses (and a right-click that cannot drag) are the
            // WM's; only the focus/raise repaint is needed.
            self.repaint_full();
            return;
        }
        // Content: focus, raise, and forward the press surface-relative.
        self.repaint_full();
        // This press goes to the surface, so its release must too: drop a
        // stale consumed bit left by a release the input queue dropped.
        self.consumed &= !button_bit;
        let (x, y) = relative(&self.surfaces, id, point);
        let body = wire::encode_pointer_down_args(&wire::PointerDownArgs { x, y, button });
        forward(
            &self.surfaces,
            &mut self.scratch,
            Some(id),
            wire::METHOD_POINTERDOWN,
            body,
        );
    }

    /// A pointer button went up.
    fn pointer_up(&mut self, button: u32) {
        let button_bit = 1u32 << (button & 31);
        self.button_down = false;
        if self.drag_session.is_some() {
            self.consumed &= !button_bit;
            self.drag_finish();
            return;
        }
        if self.consumed & button_bit != 0 {
            // The matching press was the compositor's (taskbar, window
            // button, title bar, resize frame or desktop): swallow its
            // release, and end a resize or title-bar drag it started.
            self.consumed &= !button_bit;
            if button == display::button::LEFT {
                if self.resize.is_some() {
                    self.finish_resize();
                    return;
                }
                if let Some(active) = self.drag.take() {
                    // The drag is committed, so tell the shell the new
                    // geometry.
                    if surface_by_id(&self.surfaces, active.id).is_some() {
                        self.notify_surface(active.id, wire::CHANGE_MOVED);
                    }
                }
            }
            return;
        }
        if let Some(id) = self.focused {
            let (x, y) = relative(&self.surfaces, id, self.pointer);
            let body = wire::encode_pointer_up_args(&wire::PointerUpArgs { x, y, button });
            forward(
                &self.surfaces,
                &mut self.scratch,
                Some(id),
                wire::METHOD_POINTERUP,
                body,
            );
        }
    }
}

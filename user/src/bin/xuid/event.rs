//! Input routing (issue #194 split): [`Compositor::handle_event`] turns kernel
//! pointer events into window-management actions, drag & drop routing, focus
//! changes and forwarded client events. Key events live in `keys.rs`.

use user::messenger::display::{self, wire};

use super::compositor::Compositor;
use super::layout::{cursor_rect, taskbar_hit};
use super::protocol::{Event, EventKind};
use super::surface::Drag;
use super::theme::TASKBAR_H;
use super::window::{contains, forward, raise, relative, surface_by_id};

impl Compositor {
    /// Route one input event: window-management actions first (taskbar,
    /// title-bar buttons, drag, raise), then focus/cursor updates, then
    /// forward the event to the focused surface's event endpoint. While a drag
    /// & drop session is live (issue #145) the compositor owns the pointer and
    /// routes it through the drag section instead.
    pub(super) fn handle_event(&mut self, event: Event) {
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
    /// (old and new window rectangles), empty when the window is gone.
    fn move_dragged_window(&mut self, active: Drag, pointer: (i32, i32)) -> display::Rect {
        let (screen_w, screen_h) = (self.screen.width(), self.screen.height());
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.id == active.id && !surface.minimized)
        else {
            return display::Rect::new(0, 0, 0, 0);
        };
        let window = self.surfaces[index].window();
        let target_x = pointer.0 - active.grab_x;
        let target_y = pointer.1 - active.grab_y;
        let surface = &mut self.surfaces[index];
        surface.x = target_x.clamp(0, (screen_w - window.w).max(0));
        surface.y = target_y.clamp(0, (screen_h - TASKBAR_H - window.h).max(0));
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
        // A press outside every window is a desktop click: ignore it.
        let Some((id, origin, close, minimize, title)) = self
            .surfaces
            .iter()
            .rev()
            .find(|surface| {
                !surface.desktop && !surface.minimized && contains(surface.window(), point)
            })
            .map(|surface| {
                (
                    surface.id,
                    (surface.x, surface.y),
                    surface.close_button(),
                    surface.minimize_button(),
                    surface.title_bar(),
                )
            })
        else {
            self.consumed |= button_bit;
            let over_bar = bar && point.1 >= screen_h - TASKBAR_H;
            if button == display::button::RIGHT && !over_bar {
                self.menu_open_at(point);
            }
            return;
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
        if contains(title, point) {
            self.consumed |= button_bit;
            if left {
                self.drag = Some(Drag {
                    id,
                    grab_x: point.0 - origin.0,
                    grab_y: point.1 - origin.1,
                });
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
            // button, title bar or desktop): swallow its release, and end a
            // title-bar drag it started.
            self.consumed &= !button_bit;
            if button == display::button::LEFT {
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

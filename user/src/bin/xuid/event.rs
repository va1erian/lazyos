//! Input routing (issue #194 split): [`handle_event`] turns kernel pointer and
//! key events into window-manager actions, drag & drop routing, focus changes
//! and forwarded client events, moved out of `xuid.rs` unchanged.

use alloc::vec::Vec;
use user::messenger::display::{self, wire, Canvas, Rect};

use super::drag::{drag_cancel, drag_finish, drag_move, DragSession};
use super::layout::{cursor_rect, taskbar_hit};
use super::protocol::{Event, EventKind};
use super::render::repaint;
use super::shell::{
    alt_tab_commit, alt_tab_open, modifier_key, notify_focus, notify_start_menu, notify_surface,
    taskbar_visible, AltTab, Modifiers, ShellSub,
};
use super::surface::{Drag, Surface};
use super::theme::TASKBAR_H;
use super::window::{
    close_surface, contains, cycle_focus, forward, minimize_surface, raise, relative, restore,
    surface_by_id,
};

/// Route one input event: window-management actions first (taskbar, title-bar
/// buttons, drag, raise), then focus/cursor updates, then forward the event to
/// the focused surface's event endpoint. While a drag & drop session is live
/// (issue #145) the compositor owns the pointer and routes it through the drag
/// section above instead.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_event(
    event: Event,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: &mut (i32, i32),
    focused: &mut Option<u64>,
    drag: &mut Option<Drag>,
    drag_session: &mut Option<DragSession>,
    scratch: &mut Vec<u8>,
    button_down: &mut bool,
    shell: Option<&ShellSub>,
    mods: &mut Modifiers,
    alt_tab: &mut Option<AltTab>,
    consumed: &mut u32,
) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    // `consumed` bit for the button in a press/release event.
    let button_bit = 1u32 << (event.a as u32 & 31);
    let full = Rect::new(0, 0, screen_w, screen_h);
    let bar = taskbar_visible(shell);
    match event.kind {
        EventKind::PointerMove => {
            let new = (event.a as i32, event.b as i32);
            let old = *pointer;
            // A drag & drop session owns the pointer (issue #145): the surface
            // under it gets enter/over/leave, and the source hears nothing
            // until the session ends.
            if let Some(active) = drag_session.as_mut() {
                *pointer = new;
                let damage = drag_move(active, surfaces, new, old, scratch);
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
                return;
            }
            let mut damage = cursor_rect(old).union(cursor_rect(new));
            *pointer = new;
            if let Some(active) = *drag {
                // A title-bar drag: place the window so the grabbed point
                // stays under the pointer (exact even if events were
                // coalesced), clamped to the screen and the space above the
                // taskbar.
                if let Some(index) = surfaces
                    .iter()
                    .position(|surface| surface.id == active.id && !surface.minimized)
                {
                    let window = surfaces[index].window();
                    let target_x = new.0 - active.grab_x;
                    let target_y = new.1 - active.grab_y;
                    let surface = &mut surfaces[index];
                    surface.x = target_x.clamp(0, (screen_w - window.w).max(0));
                    surface.y = target_y.clamp(0, (screen_h - TASKBAR_H - window.h).max(0));
                    damage = damage.union(window).union(surface.window());
                }
                // The matching press was consumed by the title bar, so the
                // moves stay in the compositor: the app never saw the grab.
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
                return;
            }
            // Moves are surface-relative like presses (issue #287); they go to
            // the focused surface and may fall outside it mid-drag.
            if let Some(id) = *focused {
                let (x, y) = relative(surfaces, id, new);
                let body = wire::encode_pointer_move_args(&wire::PointerMoveArgs { x, y });
                forward(surfaces, scratch, Some(id), wire::METHOD_POINTERMOVE, body);
            }
            repaint(
                screen,
                surfaces,
                *pointer,
                *focused,
                damage,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
            );
        }
        EventKind::PointerDown => {
            *button_down = true;
            if drag_session.is_some() {
                // A second press while a drag & drop session is live is
                // ignored; the session ends on the first release.
                return;
            }
            let point = *pointer;
            // The fallback taskbar paints above every window, so it hit-tests
            // first; with a shell registered it is hidden and not hit-tested.
            if bar {
                if let Some(id) = taskbar_hit(surfaces, screen_w, screen_h, point) {
                    *consumed |= button_bit;
                    let before = *focused;
                    let was_minimized =
                        surface_by_id(surfaces, id).is_some_and(|surface| surface.minimized);
                    restore(surfaces, focused, id);
                    if was_minimized {
                        if let Some(surface) = surface_by_id(surfaces, id) {
                            notify_surface(
                                shell,
                                scratch,
                                surface,
                                *focused,
                                wire::CHANGE_RESTORED,
                            );
                        }
                    }
                    if *focused != before {
                        notify_focus(shell, scratch, *focused);
                    }
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        alt_tab.as_ref(),
                    );
                    return;
                }
            }
            // A press outside every window is a desktop click: ignore it.
            let Some((id, origin, close, minimize, title)) = surfaces
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
                *consumed |= button_bit;
                return;
            };
            let before = *focused;
            raise(surfaces, id);
            *focused = Some(id);
            if *focused != before {
                notify_focus(shell, scratch, *focused);
            }
            let left = event.a as u32 == display::button::LEFT;
            if left && contains(close, point) {
                *consumed |= button_bit;
                close_surface(
                    surfaces,
                    screen,
                    *pointer,
                    focused,
                    scratch,
                    drag_session.as_ref(),
                    shell,
                    bar,
                    alt_tab.as_ref(),
                    id,
                );
                return;
            }
            if left && contains(minimize, point) {
                *consumed |= button_bit;
                minimize_surface(
                    surfaces,
                    screen,
                    *pointer,
                    focused,
                    drag_session.as_ref(),
                    shell,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                    id,
                );
                return;
            }
            if contains(title, point) {
                *consumed |= button_bit;
                if left {
                    *drag = Some(Drag {
                        id,
                        grab_x: point.0 - origin.0,
                        grab_y: point.1 - origin.1,
                    });
                }
                // Title-bar presses (and a right-click that cannot drag) are
                // the WM's; only the focus/raise repaint is needed.
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
                return;
            }
            // Content: focus, raise, and forward the press surface-relative.
            repaint(
                screen,
                surfaces,
                *pointer,
                *focused,
                full,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
            );
            // This press goes to the surface, so its release must too: drop a
            // stale consumed bit left by a release the input queue dropped.
            *consumed &= !button_bit;
            let (x, y) = relative(surfaces, id, point);
            let button = event.a as u32;
            let body = wire::encode_pointer_down_args(&wire::PointerDownArgs { x, y, button });
            forward(surfaces, scratch, Some(id), wire::METHOD_POINTERDOWN, body);
        }
        EventKind::PointerUp => {
            *button_down = false;
            if drag_session.is_some() {
                *consumed &= !button_bit;
                drag_finish(
                    drag_session,
                    surfaces,
                    screen,
                    *pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
                return;
            }
            if *consumed & button_bit != 0 {
                // The matching press was the compositor's (taskbar, window
                // button, title bar or desktop): swallow its release, and end
                // a title-bar drag it started.
                *consumed &= !button_bit;
                if event.a as u32 == display::button::LEFT {
                    if let Some(active) = drag.take() {
                        // The drag is committed, so tell the shell the new geometry.
                        if let Some(surface) = surface_by_id(surfaces, active.id) {
                            notify_surface(shell, scratch, surface, *focused, wire::CHANGE_MOVED);
                        }
                    }
                }
                return;
            }
            if let Some(id) = *focused {
                let (x, y) = relative(surfaces, id, *pointer);
                let button = event.a as u32;
                let body = wire::encode_pointer_up_args(&wire::PointerUpArgs { x, y, button });
                forward(surfaces, scratch, Some(id), wire::METHOD_POINTERUP, body);
            }
        }
        EventKind::KeyDown => {
            let key = event.a as u32;
            // Modifier keys are compositor-level (issue #167): track them and
            // never forward them to a client.
            if modifier_key(key) {
                match key {
                    display::key::SHIFT => mods.shift = true,
                    display::key::CTRL => mods.ctrl = true,
                    display::key::ALT => mods.alt = true,
                    display::key::SUPER => {
                        mods.super_key = true;
                        notify_start_menu(shell, scratch);
                    }
                    _ => {}
                }
                return;
            }
            if key == display::key::ESCAPE {
                // Escape closes the Alt+Tab overlay first...
                if alt_tab.take().is_some() {
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        None,
                    );
                    return;
                }
                // ...then it is the Ctrl+Esc start-menu chord...
                if mods.ctrl {
                    notify_start_menu(shell, scratch);
                    return;
                }
                // ...and otherwise it cancels a live drag & drop (issue #145).
                if drag_session.is_some() {
                    drag_cancel(
                        drag_session,
                        surfaces,
                        screen,
                        *pointer,
                        *focused,
                        scratch,
                        bar,
                        alt_tab.as_ref(),
                    );
                    return;
                }
            }
            if key == display::key::TAB {
                if mods.alt {
                    // Alt+Tab: the compositor's own overlay, not a client key.
                    alt_tab_open(alt_tab, surfaces, focused, screen, *pointer, bar);
                } else {
                    let before = *focused;
                    cycle_focus(surfaces, focused);
                    if *focused != before {
                        notify_focus(shell, scratch, *focused);
                    }
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        alt_tab.as_ref(),
                    );
                }
                return;
            }
            if key == display::key::F4 && mods.alt {
                // Alt+F4: ask the focused window to close, exactly like its X
                // button.
                if let Some(id) = *focused {
                    close_surface(
                        surfaces,
                        screen,
                        *pointer,
                        focused,
                        scratch,
                        drag_session.as_ref(),
                        shell,
                        bar,
                        alt_tab.as_ref(),
                        id,
                    );
                }
                return;
            }
            let body = wire::encode_key_down_args(&wire::KeyDownArgs { key });
            forward(surfaces, scratch, *focused, wire::METHOD_KEYDOWN, body);
        }
        EventKind::KeyUp => {
            let key = event.a as u32;
            if modifier_key(key) {
                match key {
                    display::key::SHIFT => mods.shift = false,
                    display::key::CTRL => mods.ctrl = false,
                    display::key::ALT => {
                        mods.alt = false;
                        // Releasing Alt commits the Alt+Tab selection.
                        if let Some(tab) = alt_tab.take() {
                            alt_tab_commit(
                                &tab, surfaces, focused, screen, *pointer, shell, scratch, bar,
                            );
                        }
                    }
                    display::key::SUPER => mods.super_key = false,
                    _ => {}
                }
                return;
            }
            // The release half of the Ctrl+Esc chord is consumed as well.
            if key == display::key::ESCAPE && mods.ctrl {
                return;
            }
            let body = wire::encode_key_up_args(&wire::KeyUpArgs { key });
            forward(surfaces, scratch, *focused, wire::METHOD_KEYUP, body);
        }
    }
}

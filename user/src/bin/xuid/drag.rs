//! Compositor-mediated drag & drop (issue #145, #194 split): the live
//! [`DragSession`] and every transition and paint it owns, moved out of
//! `xuid.rs` unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::display::{wire, Canvas, Face, Rect};

use super::layout::cursor_rect;
use super::render::repaint;
use super::shell::AltTab;
use super::surface::Surface;
use super::theme::{DRAG_ACCENT, DRAG_GHOST_BG};
use super::window::{contains, forward, relative, surface_by_id};

// ---------------------------------------------------------------------------
// Drag & drop (issue #145)
//
// The compositor owns the pointer while a drag is live: the source app hands
// over a clipboard token with `DragStart`, the surface under the pointer gets
// enter/leave/over notifications, and a release delivers `Drop` (with the
// token) or a cancelled `DragEnded`. All transitions live in this section;
// `handle_event`, `handle_request` and `repaint` only route into them, which
// keeps the window-management paths separate.
// ---------------------------------------------------------------------------

/// An active drag & drop session started by a client's `DragStart`.
pub(super) struct DragSession {
    /// Surface whose client started the drag.
    pub(super) source: u64,
    /// Clipboard token delivered to the drop target.
    pub(super) token: u64,
    /// MIME type of the token's payload; drawn as the drag label.
    pub(super) mime: String,
    /// Surface currently under the pointer, if any (never the source).
    pub(super) target: Option<u64>,
}
/// The topmost visible surface whose content contains `point`, ignoring
/// `source` and the desktop.
fn drag_target_at(surfaces: &[Surface], source: u64, point: (i32, i32)) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| {
            !surface.minimized
                && !surface.desktop
                && surface.id != source
                && contains(surface.content(), point)
        })
        .map(|surface| surface.id)
}

/// The rectangle the drag ghost occupies around `point`.
fn ghost_rect(mime: &str, point: (i32, i32)) -> Rect {
    let shown = mime.char_indices().nth(24).map_or(mime, |(end, _)| &mime[..end]);
    let label = Face::Sans.width(shown) + 8;
    Rect::new(point.0 + 6, point.1 + 6, 14 + label, 16)
}

/// Tell surface `id` a drag carrying `mime` entered at `point`.
fn send_enter(surfaces: &[Surface], scratch: &mut Vec<u8>, id: u64, point: (i32, i32), mime: &str) {
    let (x, y) = relative(surfaces, id, point);
    let args = wire::DragEnterArgs {
        x,
        y,
        mime: mime.into(),
    };
    let body = wire::encode_drag_enter_args(&args);
    forward(surfaces, scratch, Some(id), wire::METHOD_DRAGENTER, body);
}

/// Tell surface `id` a drag left it.
fn send_leave(surfaces: &[Surface], scratch: &mut Vec<u8>, id: u64) {
    forward(
        surfaces,
        scratch,
        Some(id),
        wire::METHOD_DRAGLEAVE,
        Ok(Vec::new()),
    );
}

/// Tell the drag's source it ended (`dropped`) or was cancelled.
fn send_ended(surfaces: &[Surface], scratch: &mut Vec<u8>, source: u64, dropped: bool) {
    let body = wire::encode_drag_ended_args(&wire::DragEndedArgs { dropped });
    forward(
        surfaces,
        scratch,
        Some(source),
        wire::METHOD_DRAGENDED,
        body,
    );
}

/// Start a drag from `source`: adopt the token/mime, greet a surface already
/// under the pointer, and draw the ghost.
#[allow(clippy::too_many_arguments)]
pub(super) fn drag_begin(
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
    source: u64,
    token: u64,
    mime: String,
) {
    let mut active = DragSession {
        source,
        token,
        mime,
        target: None,
    };
    if let Some(id) = drag_target_at(surfaces, source, pointer) {
        send_enter(surfaces, scratch, id, pointer, &active.mime);
        active.target = Some(id);
    }
    let damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    *drag = Some(active);
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
}

/// Route a pointer move while a drag is live: the surface under the pointer
/// gets enter/over/leave; the source gets nothing until the drag ends.
/// Returns the damage the move needs.
pub(super) fn drag_move(
    drag: &mut DragSession,
    surfaces: &[Surface],
    pointer: (i32, i32),
    old: (i32, i32),
    scratch: &mut Vec<u8>,
) -> Rect {
    let mut damage = cursor_rect(old)
        .union(cursor_rect(pointer))
        .union(ghost_rect(&drag.mime, old))
        .union(ghost_rect(&drag.mime, pointer));
    let next = drag_target_at(surfaces, drag.source, pointer);
    if next != drag.target {
        if let Some(id) = drag.target {
            send_leave(surfaces, scratch, id);
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
        drag.target = next;
        if let Some(id) = next {
            send_enter(surfaces, scratch, id, pointer, &drag.mime);
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
    } else if let Some(id) = next {
        let (x, y) = relative(surfaces, id, pointer);
        let body = wire::encode_drag_over_args(&wire::DragOverArgs { x, y });
        forward(surfaces, scratch, Some(id), wire::METHOD_DRAGOVER, body);
    }
    damage
}

/// Finish a drag at `pointer`: `Drop` the token on the surface under it, or
/// send `DragLeave` and a cancelled `DragEnded`.
#[allow(clippy::too_many_arguments)]
pub(super) fn drag_finish(
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
) {
    let Some(active) = drag.take() else {
        return;
    };
    let mut damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    match drag_target_at(surfaces, active.source, pointer) {
        Some(id) => {
            let (x, y) = relative(surfaces, id, pointer);
            let args = wire::DropArgs {
                x,
                y,
                token: active.token,
                mime: active.mime.clone(),
            };
            let body = wire::encode_drop_args(&args);
            forward(surfaces, scratch, Some(id), wire::METHOD_DROP, body);
            send_ended(surfaces, scratch, active.source, true);
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
        None => {
            drag_leave_target(&active, surfaces, scratch, &mut damage);
            send_ended(surfaces, scratch, active.source, false);
        }
    }
    if let Some(surface) = surface_by_id(surfaces, active.source) {
        damage = damage.union(surface.window());
    }
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
}

/// Cancel a live drag (Escape, `DragCancel`, or the surface going away).
#[allow(clippy::too_many_arguments)]
pub(super) fn drag_cancel(
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
) {
    let Some(active) = drag.take() else {
        return;
    };
    let mut damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    drag_leave_target(&active, surfaces, scratch, &mut damage);
    send_ended(surfaces, scratch, active.source, false);
    if let Some(surface) = surface_by_id(surfaces, active.source) {
        damage = damage.union(surface.window());
    }
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
}

/// Notify a drag's current target that the drag left, growing `damage`.
fn drag_leave_target(
    active: &DragSession,
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    damage: &mut Rect,
) {
    if let Some(id) = active.target {
        send_leave(surfaces, scratch, id);
        if let Some(surface) = surface_by_id(surfaces, id) {
            *damage = damage.union(surface.window());
        }
    }
}

/// Paint the active drag & drop session over the composited frame: a frame
/// around the target surface and a payload ghost at the cursor.
pub(super) fn draw_drag(
    screen: &mut Canvas,
    surfaces: &[Surface],
    session: &DragSession,
    pointer: (i32, i32),
    clip: Rect,
) {
    if let Some(surface) = session.target.and_then(|id| surface_by_id(surfaces, id)) {
        let content = surface.content();
        screen.fill(
            Rect::new(content.x, content.y, content.w, 3),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x, content.y + content.h - 3, content.w, 3),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x, content.y, 3, content.h),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x + content.w - 3, content.y, 3, content.h),
            clip,
            DRAG_ACCENT,
        );
    }
    let ghost = ghost_rect(&session.mime, pointer);
    screen.fill(Rect::new(ghost.x, ghost.y, 14, 14), clip, DRAG_ACCENT);
    screen.fill(
        Rect::new(ghost.x + 3, ghost.y + 3, 8, 8),
        clip,
        DRAG_GHOST_BG,
    );
    let label = Rect::new(ghost.x + 14, ghost.y + 2, ghost.w - 14, 12);
    screen.fill(label, clip, DRAG_GHOST_BG);
    // `ghost_rect` reserves room for 24 characters but a MIME string may be
    // up to `display::MAX_MIME`; clip the text to the label so glyphs past it
    // (outside the drag's damage) cannot leave trails as the pointer moves.
    screen.text_face(
        ghost.x + 18,
        ghost.y + (16 - Face::Sans.height()) / 2,
        &session.mime,
        Face::Sans,
        DRAG_ACCENT,
        clip.intersect(label),
    );
}

//! The `dragdemo` source role (issue #194 split): offer a typed payload through
//! `clipboardd`, draw the draggable item, and turn a press that travels past
//! [`DRAG_THRESHOLD`] into a compositor-owned drag.
//!
//! Split out of `dragdemo.rs`; the code is unchanged.

use alloc::format;
use user::messenger::display::{self, Canvas, Color, DragKind, Event, Rect};
use user::messenger::{errno, Error};
use user::sys;

use super::app::{attach, connect_clipboard, connect_display, is_timeout, App};
use super::{DEMO_MIME, DRAG_THRESHOLD, H, ITEM, PAYLOAD, W};

/// The drag source's state.
struct SourceState {
    /// Clipboard token offered at startup; `None` without `clipboardd`.
    token: Option<u64>,
    /// The compositor owns the pointer from `DragStart` until `DragEnded`.
    dragging: bool,
    /// Where the press landed inside the content, if any.
    press: Option<(i64, i64)>,
    /// Last pointer position the compositor reported.
    pointer: Option<(i64, i64)>,
    /// Status word drawn at the bottom.
    status: &'static str,
}

/// Draw the source's window: the draggable item and its state.
fn source_draw(canvas: &mut Canvas, state: &SourceState) {
    let clip = Rect::new(0, 0, W, H);
    canvas.fill(clip, clip, Color::rgb(12, 14, 24));
    canvas.text(8, 6, "DRAG SOURCE", Color::rgb(240, 244, 255), clip, 1);
    let item = if state.dragging {
        Color::rgb(120, 170, 240)
    } else {
        Color::rgb(200, 70, 70)
    };
    canvas.fill(ITEM, clip, item);
    canvas.fill(
        Rect::new(ITEM.x + 6, ITEM.y + 6, ITEM.w - 12, ITEM.h - 12),
        clip,
        Color::rgb(245, 245, 250),
    );
    canvas.fill(
        Rect::new(ITEM.x + 12, ITEM.y + 12, ITEM.w - 24, ITEM.h - 24),
        clip,
        item,
    );
    canvas.text(
        ITEM.x - 12,
        ITEM.y + ITEM.h + 4,
        "DRAG THIS ITEM",
        Color::rgb(230, 200, 120),
        clip,
        1,
    );
    let token = state.token.map(|token| token as i64).unwrap_or(-1);
    canvas.text(
        8,
        H - 30,
        &format!("TOKEN {token}"),
        Color::rgb(200, 210, 230),
        clip,
        1,
    );
    canvas.text(8, H - 16, state.status, Color::rgb(160, 230, 180), clip, 1);
    if let Some((x, y)) = state.pointer {
        canvas.fill(
            Rect::new(x as i32 - 2, y as i32 - 2, 5, 5),
            clip,
            Color::rgb(255, 255, 255),
        );
    }
}

/// The drag source: offer a payload, then turn a press-and-move into a drag.
pub(super) fn source() -> ! {
    sys::write_str("dragdemo: drag source\n");
    let clipboard = connect_clipboard();
    let mut app = attach(connect_display(), "dragsrc", &|canvas| {
        source_draw(canvas, &SourceState::default())
    });
    let token = clipboard
        .as_ref()
        .and_then(|client| client.copy("dragdemo", &[(DEMO_MIME, PAYLOAD)]).ok());
    match token {
        Some(token) => sys::write_str(&format!("DND:SOURCE:PASS token={token}\n")),
        None => sys::write_str("DND:SKIP:no clipboardd offer\n"),
    }
    let mut state = SourceState {
        token,
        dragging: false,
        press: None,
        pointer: None,
        status: "READY",
    };
    app.redraw(&|canvas| source_draw(canvas, &state));
    loop {
        match app
            .events
            .recv_with(&mut app.buffer, Some(sys::clock() + 1))
        {
            Ok(message) => {
                if let Some(event) = display::decode_event(&message) {
                    handle_source_input(&mut app, &mut state, event);
                } else if let Some(event) = display::decode_drag_event(&message) {
                    if event.kind == DragKind::Ended {
                        state.dragging = false;
                        state.press = None;
                        if event.dropped {
                            state.status = "DROPPED";
                        } else {
                            state.status = "CANCELLED";
                            sys::write_str("DND:CANCEL:PASS\n");
                        }
                        app.redraw(&|canvas| source_draw(canvas, &state));
                    }
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(Error::Errno(code)) if code == -errno::EPIPE => sys::exit(0),
            Err(_) => {}
        }
    }
}

/// Fold one input event into the source state; start the drag past threshold.
fn handle_source_input(app: &mut App, state: &mut SourceState, event: Event) {
    match event {
        Event::PointerDown { x, y, .. } => {
            let (x, y) = (x as i64, y as i64);
            if in_item(x, y) {
                state.press = Some((x, y));
                state.status = "PRESSED";
                app.redraw(&|canvas| source_draw(canvas, state));
            }
        }
        Event::PointerMove { x, y } => {
            let (x, y) = (x as i64, y as i64);
            state.pointer = Some((x, y));
            if !state.dragging {
                if let (Some((px, py)), Some(token)) = (state.press, state.token) {
                    if (x - px).abs() >= DRAG_THRESHOLD || (y - py).abs() >= DRAG_THRESHOLD {
                        match app.display.drag_start(app.surface, token, DEMO_MIME) {
                            Ok(()) => {
                                state.dragging = true;
                                state.status = "DRAGGING";
                                sys::write_str(&format!(
                                    "DND:START:PASS token={token} mime={DEMO_MIME}\n"
                                ));
                            }
                            Err(_) => {
                                state.press = None;
                                sys::write_str("DND:START:FAIL:compositor refused\n");
                            }
                        }
                    }
                }
            }
            app.redraw(&|canvas| source_draw(canvas, state));
        }
        Event::PointerUp { .. } => {
            state.press = None;
            if !state.dragging {
                state.status = "READY";
            }
            app.redraw(&|canvas| source_draw(canvas, state));
        }
        Event::PointerWheel { .. } | Event::KeyDown { .. } | Event::KeyUp { .. } => {}
    }
}

impl Default for SourceState {
    fn default() -> SourceState {
        SourceState {
            token: None,
            dragging: false,
            press: None,
            pointer: None,
            status: "READY",
        }
    }
}

/// Whether the surface-local point is inside the draggable item.
fn in_item(x: i64, y: i64) -> bool {
    x >= ITEM.x as i64
        && y >= ITEM.y as i64
        && x < (ITEM.x + ITEM.w) as i64
        && y < (ITEM.y + ITEM.h) as i64
}

//! The `dragdemo` target role (issue #194 split): accept the compositor's drag
//! events, paste the dropped token back through `clipboardd`, and run the
//! cross-session denial probe.
//!
//! Split out of `dragdemo.rs`; the code is unchanged.

use alloc::format;
use alloc::string::String;
use user::messenger::display::{self, Canvas, Color, DragKind, Rect};
use user::messenger::{clipboard, errno, Error};
use user::sys;

use super::app::{attach, connect_clipboard, connect_display, is_timeout, App};
use super::{H, PAYLOAD, PROBE_TICKS, W};

/// The drop target's state.
struct TargetState {
    /// The compositor reports the pointer over this surface.
    hover: bool,
    /// Status word drawn in the drop zone.
    status: &'static str,
    /// MIME type of the last received payload.
    mime: String,
    /// Payload length of the last received token, or -1 before a drop.
    size: i64,
}

/// Draw the target's window: the drop zone and the last payload's type/len.
fn target_draw(canvas: &mut Canvas, state: &TargetState) {
    let clip = Rect::new(0, 0, W, H);
    canvas.fill(clip, clip, Color::rgb(12, 14, 24));
    canvas.text(8, 6, "DROP TARGET", Color::rgb(240, 244, 255), clip, 1);
    let zone = Rect::new(16, 34, W - 32, 86);
    let border = if state.hover {
        Color::rgb(245, 196, 84)
    } else {
        Color::rgb(92, 106, 152)
    };
    canvas.fill(zone, clip, Color::rgb(18, 24, 38));
    canvas.fill(Rect::new(zone.x, zone.y, zone.w, 2), clip, border);
    canvas.fill(
        Rect::new(zone.x, zone.y + zone.h - 2, zone.w, 2),
        clip,
        border,
    );
    canvas.fill(Rect::new(zone.x, zone.y, 2, zone.h), clip, border);
    canvas.fill(
        Rect::new(zone.x + zone.w - 2, zone.y, 2, zone.h),
        clip,
        border,
    );
    canvas.text(
        zone.x + 12,
        zone.y + 16,
        "DROP HERE",
        Color::rgb(200, 210, 230),
        clip,
        1,
    );
    canvas.text(
        zone.x + 12,
        zone.y + 34,
        state.status,
        Color::rgb(150, 160, 190),
        clip,
        1,
    );
    if state.size >= 0 {
        canvas.text(8, H - 46, &state.mime, Color::rgb(230, 200, 120), clip, 1);
        canvas.text(
            8,
            H - 30,
            &format!("{} BYTES RECEIVED", state.size),
            Color::rgb(160, 230, 180),
            clip,
            1,
        );
    }
}

/// The drop target: accept the compositor's drag events and paste the token.
pub(super) fn target() -> ! {
    sys::write_str("dragdemo: drop target\n");
    let clipboard = connect_clipboard();
    let mut app = attach(connect_display(), "dragtgt", &|canvas| {
        target_draw(canvas, &TargetState::default())
    });
    let mut state = TargetState::default();
    sys::write_str("DND:TARGET:PASS\n");
    loop {
        match app
            .events
            .recv_with(&mut app.buffer, Some(sys::clock() + 1))
        {
            Ok(message) => {
                if let Some(event) = display::decode_drag_event(&message) {
                    match event.kind {
                        DragKind::Enter => {
                            state.hover = true;
                            state.status = "RELEASE TO SEND";
                            state.mime = event.mime.clone();
                            app.redraw(&|canvas| target_draw(canvas, &state));
                        }
                        DragKind::Leave => {
                            state.hover = false;
                            state.status = "WAITING FOR DRAG";
                            app.redraw(&|canvas| target_draw(canvas, &state));
                        }
                        DragKind::Drop => {
                            state.hover = false;
                            receive_drop(&mut app, &mut state, &clipboard, &event);
                        }
                        DragKind::Over | DragKind::Ended => {}
                    }
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(Error::Errno(code)) if code == -errno::EPIPE => sys::exit(0),
            Err(_) => {}
        }
    }
}

/// Paste the token the compositor dropped, verify it, and probe the
/// cross-session deny path.
fn receive_drop(
    app: &mut App,
    state: &mut TargetState,
    clipboard: &Option<clipboard::Client>,
    event: &display::DragEvent,
) {
    let Some(client) = clipboard else {
        state.status = "NO CLIPBOARD";
        sys::write_str("DND:DROP:FAIL:no clipboardd\n");
        app.redraw(&|canvas| target_draw(canvas, state));
        return;
    };
    match client.paste_token(event.token, &event.mime) {
        Ok(bytes) => {
            state.mime = event.mime.clone();
            state.size = bytes.len() as i64;
            if bytes == PAYLOAD {
                state.status = "RECEIVED";
                sys::write_str(&format!(
                    "DND:DROP:PASS token={} mime={} bytes={}\n",
                    event.token,
                    event.mime,
                    bytes.len()
                ));
            } else {
                state.status = "MISMATCH";
                sys::write_str("DND:DROP:FAIL:payload mismatch\n");
            }
        }
        Err(error) => {
            state.status = "DENIED";
            sys::write_str(&format!("DND:DROP:FAIL:paste {}\n", error.message()));
        }
    }
    app.redraw(&|canvas| target_draw(canvas, state));
    denial_probe(event.token, &event.mime);
}

/// A drop must not bypass the clipboard's session scope. The probe switches
/// credentials, which cannot be undone once privilege is dropped, so it runs in
/// a short-lived child (same session, so it can resolve `clipboardd` first) and
/// the target just reaps it.
fn denial_probe(token: u64, mime: &str) {
    let token = format!("{token}");
    let Some(pid) = sys::spawn_native(fhs::bin::DRAGDEMO, &["probe", &token, mime]) else {
        sys::write_str("DND:DENIED:FAIL:could not start the probe\n");
        return;
    };
    let deadline = sys::clock() + PROBE_TICKS;
    while sys::clock() < deadline {
        if let Some((reaped, _)) = sys::wait(deadline) {
            if reaped == pid {
                return;
            }
        }
    }
    sys::write_str("DND:DENIED:FAIL:the probe did not exit\n");
}

impl Default for TargetState {
    fn default() -> TargetState {
        TargetState {
            hover: false,
            status: "WAITING FOR DRAG",
            mime: String::new(),
            size: -1,
        }
    }
}

//! The compositor-side state of a client-mode window (`xuid` surface).
//!
//! One [`ClientWindow`] owns the handles of one surface — its event endpoint
//! and the shared pixel buffers presented through it ([`Slots`]) — so the
//! backend can keep a
//! `ClientWindow` **per open window** and support several at once (the Files
//! explorer opens one window per folder). The connection shared by all of them
//! is [`ClientState`].
//!
//! Handle transfers MOVE the handle (`kernel/src/ipc/channels.rs`), so each
//! surface gets a fresh event pair: reusing one peer for a second
//! `CreateSurface` would send a handle this task no longer owns. Every failure
//! path releases what was created so far, so no half-built surface stays in the
//! compositor with nothing able to destroy it.

use std::sync::atomic::{AtomicBool, Ordering};

use messenger_generated::os_lazy_display_v1 as wire;

use crate::display::{self, Client, Event};
use crate::input;
use crate::sys::{self, errno};

mod slots;

pub use slots::{copy_rect, Slots};

/// How many times the first attach is retried at a newer compositor size.
const ATTACH_RETRIES: usize = 4;
/// Ticks to wait for the `Configure` that explains a refused first attach.
const CONFIGURE_WAIT_TICKS: u64 = 5;
/// Receive buffer for the event drain; events are far smaller.
const EVENT_BYTES: usize = 4096;
/// Most events kept for replay; a flood of pointer moves must not grow it.
const MAX_KEPT_EVENTS: usize = 256;

/// What kind of surface a window is (`os.lazy.display.v1` `Role`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceRole {
    /// A decorated, focusable app window (every app but the shell).
    #[default]
    Window,
    /// The full-screen desktop under every window (shell only).
    Desktop,
    /// A chromeless panel above every window at screen `(x, y)` (shell only):
    /// placed before its first commit, never focused, no keyboard session.
    Panel { x: i32, y: i32 },
}

impl SurfaceRole {
    /// The wire `Role` value.
    pub const fn wire(self) -> u32 {
        match self {
            SurfaceRole::Window => wire::ROLE_WINDOW,
            SurfaceRole::Desktop => wire::ROLE_DESKTOP,
            SurfaceRole::Panel { .. } => wire::ROLE_PANEL,
        }
    }
}

/// A live compositor connection, shared by a backend's windows.
pub struct ClientState {
    /// The compositor endpoint (`Client::connect`).
    pub client: Client,
}

impl ClientState {
    /// A connection with no window yet.
    pub fn new(client: Client) -> ClientState {
        ClientState { client }
    }
}

/// Why [`ClientWindow::open`] gave no window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The window was closed before its first buffer was attached (a close
    /// request arrived, or the compositor no longer knows the surface; issue
    /// #498). Not a failure: the user dismissed the window, so the app should
    /// exit cleanly.
    Closed,
    /// Anything else, described for the log.
    Failed(String),
}

/// Set when [`ClientWindow::open`] ended with [`OpenError::Closed`]. The
/// backend reports it to `xui_core` as a plain error, so the app's `run`
/// outcome cannot say which kind it was; the process asks here instead
/// ([`closed_while_opening`], `launch::finish`).
static CLOSED_WHILE_OPENING: AtomicBool = AtomicBool::new(false);

/// Note that a window was closed before it opened (see [`OpenError::Closed`]).
pub(crate) fn note_closed_while_opening() {
    CLOSED_WHILE_OPENING.store(true, Ordering::Relaxed);
}

/// Whether an opening window was closed before its first attach.
pub fn closed_while_opening() -> bool {
    CLOSED_WHILE_OPENING.load(Ordering::Relaxed)
}

impl core::fmt::Display for OpenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OpenError::Closed => f.write_str("window closed while opening"),
            OpenError::Failed(message) => f.write_str(message),
        }
    }
}

impl From<String> for OpenError {
    fn from(message: String) -> OpenError {
        OpenError::Failed(message)
    }
}

/// One open window's surface: the event channel and the buffer slots.
pub struct ClientWindow {
    /// This task's end of the surface's event channel.
    pub events: u64,
    /// The surface id from `CreateSurface`.
    pub surface: u64,
    /// The shared pixel buffers; closed with the surface.
    pub slots: Slots,
    /// The surface size in pixels.
    pub rect: (i32, i32),
    /// The title the compositor currently shows, so an unchanged title is not
    /// sent again (the Editor retitles on every keystroke).
    pub title: String,
    /// The window's keyboard session with `inputd`, when the service is
    /// running; without it keys come from the compositor's legacy events.
    pub input: Option<input::Session>,
    /// The `Configure` that [`ClientWindow::open`] consumed while sizing the
    /// first buffer. The app has not seen it yet, so the backend replays it as
    /// a `Resize` on the first tick.
    pub pending_configure: Option<(i32, i32)>,
    /// Key and pointer events that arrived while `open` waited for that
    /// `Configure`; replayed after the `Resize` so no startup input is lost.
    pub pending_events: Vec<Event>,
}

impl ClientWindow {
    /// Create the window's surface bound to `client` with its own buffer and
    /// event channel. A panel is placed before anything is committed, so it
    /// never flashes at the compositor's default `(0, 0)`.
    pub fn open(
        client: Client,
        width: u32,
        height: u32,
        title: &str,
        role: SurfaceRole,
    ) -> Result<ClientWindow, OpenError> {
        let (events, peer) =
            sys::msg_create_pair().map_err(|code| format!("event pair: errno {code}"))?;
        let created =
            client.create_surface_with_role(width as u64, height as u64, title, peer, role.wire());
        let surface = match created {
            Ok(surface) => surface,
            Err(code) => {
                // The peer may or may not have been consumed; closing a stale
                // handle only fails harmlessly.
                let _ = display::close(peer);
                let _ = display::close(events);
                return Err(format!("create_surface: errno {code}").into());
            }
        };
        if let SurfaceRole::Panel { x, y } = role {
            if let Err(code) = client.place_surface(surface, x, y) {
                let _ = client.destroy_surface(surface);
                let _ = display::close(events);
                return Err(format!("place_surface: errno {code}").into());
            }
        }
        let first = match attach_first_buffer(client, events, surface, width, height) {
            Ok(first) => first,
            Err(error) => {
                let _ = client.destroy_surface(surface);
                let _ = display::close(events);
                return Err(error);
            }
        };
        let (width, height) = first.size;
        let mut slots = Slots::new(first.buffer, first.va, width as i32, height as i32);
        if let Err(code) = slots.reserve(client, surface) {
            slots.close();
            let _ = client.destroy_surface(surface);
            let _ = display::close(events);
            return Err(if code == -errno::ENOENT {
                OpenError::Closed
            } else {
                OpenError::Failed(format!("attach_slot: errno {code}"))
            });
        }
        // Best effort: `xuid` registered the surface with `inputd` before it
        // answered `CreateSurface`, so the session can be opened right away.
        // The desktop and panels never take keyboard focus.
        let input = (role == SurfaceRole::Window)
            .then(|| input::Session::open(surface).ok())
            .flatten();
        Ok(ClientWindow {
            events,
            surface,
            slots,
            rect: (width as i32, height as i32),
            title: title.to_owned(),
            input,
            pending_configure: first.resized,
            pending_events: first.events,
        })
    }

    /// Follow a `Configure` to `width` x `height`. The buffers catch up as
    /// each slot is next drawn into ([`Slots::acquire`]): the slot the
    /// compositor reads cannot be replaced, and until a new-size frame is
    /// presented the compositor shows the old one cropped or padded.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.rect = (width, height);
    }

    /// Destroy the surface and close its event channel and buffer. Safe to call
    /// on a window already torn down (every handle is zeroed).
    pub fn close(&self, client: Client) {
        if let Some(session) = &self.input {
            session.close();
        }
        if self.surface != 0 {
            let _ = client.destroy_surface(self.surface);
        }
        if self.events != 0 {
            let _ = display::close(self.events);
        }
        // The compositor holds its own reference to the attached buffers,
        // so this only releases the client's mappings and quota charge.
        self.slots.close();
    }
}

/// Why [`attach_new_buffer`] failed.
enum AttachError {
    /// The compositor answered `EINVAL`: it no longer has a surface of this
    /// size (a maximize or resize landed before the attach), or it has gone.
    Refused,
    /// The compositor answered `ENOENT`: the surface is gone, the window was
    /// closed before the attach (issue #498).
    Closed,
    /// Anything else (no buffer quota, a dead compositor): not retryable.
    Failed(String),
}

/// Allocate a shared buffer, attach it as slot 0 of `surface`, return its
/// handle and mapping. A failed attach closes the buffer again so it does not
/// count against the per-process quota until task exit.
fn attach_new_buffer(client: Client, surface: u64, size: u64) -> Result<(u64, u64), AttachError> {
    let (buffer, va, _) = sys::buffer_create(size)
        .map_err(|code| AttachError::Failed(format!("create_buffer: errno {code}")))?;
    if let Err(code) = client.attach_slot(surface, 0, buffer, size) {
        let _ = sys::buffer_close(buffer);
        return Err(match code {
            c if c == -errno::EINVAL => AttachError::Refused,
            c if c == -errno::ENOENT => AttachError::Closed,
            _ => AttachError::Failed(format!("attach_slot: errno {code}")),
        });
    }
    Ok((buffer, va))
}

/// The first buffer of a new window.
struct FirstBuffer {
    buffer: u64,
    va: u64,
    /// The size it was attached at, which is the surface's size right now.
    size: (u32, u32),
    /// The `Configure` consumed to learn that size, if the surface changed.
    resized: Option<(i32, i32)>,
    /// Input drained while waiting for the `Configure`, oldest first.
    events: Vec<Event>,
}

/// Attach the window's first buffer. The compositor may resize the new
/// surface (maximize, a resize drag, a size request) between `CreateSurface`
/// and this attach, and then refuses a buffer sized for the old geometry.
/// `Configure` carries the surface's current size, so follow it and retry,
/// the same recovery a live window gets from the next `Configure`.
fn attach_first_buffer(
    client: Client,
    events: u64,
    surface: u64,
    mut width: u32,
    mut height: u32,
) -> Result<FirstBuffer, OpenError> {
    let mut resized = None;
    let mut kept = Vec::new();
    for _ in 0..ATTACH_RETRIES {
        let size = width as u64 * height as u64 * 4;
        match attach_new_buffer(client, surface, size) {
            Ok((buffer, va)) => {
                return Ok(FirstBuffer {
                    buffer,
                    va,
                    size: (width, height),
                    resized,
                    events: kept,
                })
            }
            Err(AttachError::Closed) => return Err(OpenError::Closed),
            Err(AttachError::Failed(message)) => return Err(OpenError::Failed(message)),
            Err(AttachError::Refused) => match newest_configure(events, &mut kept)? {
                Some((w, h)) if w > 0 && h > 0 => {
                    (width, height) = (w as u32, h as u32);
                    resized = Some((w, h));
                }
                _ => break,
            },
        }
    }
    Err(OpenError::Failed(format!(
        "attach_slot: errno {}",
        -errno::EINVAL
    )))
}

/// The newest `Configure` queued on `events`, waiting briefly for the first
/// one. Other events are pushed to `kept` (bounded) for the backend to replay,
/// since `xuid` focuses a new surface before `CreateSurface` returns and early
/// keystrokes can be queued here. A close request is [`OpenError::Closed`]:
/// the window is already gone.
fn newest_configure(events: u64, kept: &mut Vec<Event>) -> Result<Option<(i32, i32)>, OpenError> {
    let mut buf = [0u8; EVENT_BYTES];
    let mut newest = None;
    let mut wait = CONFIGURE_WAIT_TICKS;
    loop {
        let deadline = sys::clock_ticks().saturating_add(wait);
        match sys::msg_recv(events, &mut buf, deadline) {
            Ok(result) => {
                let Some(parcel) = display::decode_message(&buf[..result.bytes as usize]) else {
                    continue;
                };
                if parcel.header.method == display::METHOD_WINDOW_CLOSE {
                    return Err(OpenError::Closed);
                }
                match display::decode_event(&parcel) {
                    Some(Event::Configure { width, height, .. }) => newest = Some((width, height)),
                    Some(event) => keep_event(kept, event),
                    None => {}
                }
                // Anything still queued is already here; do not wait again.
                wait = 0;
            }
            Err(code) if code == -errno::ETIMEDOUT => return Ok(newest),
            Err(code) => return Err(OpenError::Failed(format!("event drain: errno {code}"))),
        }
    }
}

/// Keep `event` for replay, bounded by [`MAX_KEPT_EVENTS`]. When full, pointer
/// motion is the expendable input: a new move is dropped, and any other event
/// (a key or button transition, which cannot be reconstructed) evicts the
/// oldest queued move to make room.
fn keep_event(kept: &mut Vec<Event>, event: Event) {
    if kept.len() < MAX_KEPT_EVENTS {
        kept.push(event);
    } else if matches!(event, Event::PointerMove { .. }) {
        // Dropped: the next move supersedes it.
    } else if let Some(at) = kept
        .iter()
        .position(|kept| matches!(kept, Event::PointerMove { .. }))
    {
        kept.remove(at);
        kept.push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: i32) -> Event {
        Event::PointerMove { x, y: 0 }
    }

    /// A failed run exits 1, but once a window was closed while opening the
    /// same error exits 0 (issue #498). The only test that sets the flag.
    #[test]
    fn a_window_closed_while_opening_exits_cleanly() {
        let failed = |_: ()| Err::<(), _>(OpenError::Failed("create_surface: errno 5".into()));
        assert_eq!(crate::launch::finish("T", Ok::<(), OpenError>(())), 0);
        assert_eq!(crate::launch::finish("T", failed(())), 1);
        assert!(!closed_while_opening());

        note_closed_while_opening();
        assert!(closed_while_opening());
        assert_eq!(crate::launch::finish("T", Err(OpenError::Closed)), 0);
        assert_eq!(OpenError::Closed.to_string(), "window closed while opening");
        // The shape `open_window` really reports: the close text wrapped by the
        // backend, so a change to the text or the wrapper fails here.
        let wrapped = xui_core::backend::BackendError::Other(OpenError::Closed.to_string());
        assert_eq!(crate::launch::finish("T", Err(wrapped)), 0);
    }

    #[test]
    fn a_full_buffer_drops_moves_but_keeps_key_transitions() {
        let mut kept: Vec<Event> = (0..MAX_KEPT_EVENTS as i32).map(mv).collect();
        keep_event(&mut kept, mv(999));
        assert_eq!(kept.len(), MAX_KEPT_EVENTS);
        assert!(!kept.contains(&mv(999)), "a new move is dropped when full");

        keep_event(&mut kept, Event::KeyDown { key: 7 });
        assert_eq!(kept.len(), MAX_KEPT_EVENTS);
        assert_eq!(kept.last(), Some(&Event::KeyDown { key: 7 }));
        assert!(!kept.contains(&mv(0)), "the oldest move made room");
    }

    #[test]
    fn with_no_move_to_evict_a_full_buffer_is_left_alone() {
        let mut kept: Vec<Event> = (0..MAX_KEPT_EVENTS as u32)
            .map(|key| Event::KeyDown { key })
            .collect();
        keep_event(&mut kept, Event::KeyUp { key: 1 });
        assert_eq!(kept.len(), MAX_KEPT_EVENTS);
        assert!(!kept.contains(&Event::KeyUp { key: 1 }));
    }
}

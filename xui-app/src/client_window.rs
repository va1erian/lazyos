//! The compositor-side state of a client-mode window (`xuid` surface).
//!
//! One [`ClientWindow`] owns the handles of one surface — its event endpoint,
//! the shared pixel buffer and its mapping — so the backend can keep a
//! `ClientWindow` **per open window** and support several at once (the Files
//! explorer opens one window per folder). The connection shared by all of them
//! is [`ClientState`].
//!
//! Handle transfers MOVE the handle (`kernel/src/ipc/channels.rs`), so each
//! surface gets a fresh event pair: reusing one peer for a second
//! `CreateSurface` would send a handle this task no longer owns. Every failure
//! path releases what was created so far, so no half-built surface stays in the
//! compositor with nothing able to destroy it.

use crate::display::{self, Client, Event};
use crate::input;
use crate::sys::{self, errno};

/// How many times the first attach is retried at a newer compositor size.
const ATTACH_RETRIES: usize = 4;
/// Ticks to wait for the `Configure` that explains a refused first attach.
const CONFIGURE_WAIT_TICKS: u64 = 5;
/// Receive buffer for the event drain; events are far smaller.
const EVENT_BYTES: usize = 4096;

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

/// One open window's surface: the event channel, the attached buffer and its
/// mapping.
pub struct ClientWindow {
    /// This task's end of the surface's event channel.
    pub events: u64,
    /// The surface id from `CreateSurface`.
    pub surface: u64,
    /// The shared pixel buffer handle; closed with the surface.
    pub buffer: u64,
    /// The shared pixel buffer mapping.
    pub va: u64,
    /// The mapping length in bytes.
    pub size: u64,
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
}

impl ClientWindow {
    /// Create the window's surface bound to `client` with its own buffer and
    /// event channel.
    pub fn open(
        client: Client,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<ClientWindow, String> {
        let (events, peer) =
            sys::msg_create_pair().map_err(|code| format!("event pair: errno {code}"))?;
        let surface = match client.create_surface(width as u64, height as u64, title, peer) {
            Ok(surface) => surface,
            Err(code) => {
                // The peer may or may not have been consumed; closing a stale
                // handle only fails harmlessly.
                let _ = display::close(peer);
                let _ = display::close(events);
                return Err(format!("create_surface: errno {code}"));
            }
        };
        let first = match attach_first_buffer(client, events, surface, width, height) {
            Ok(first) => first,
            Err(message) => {
                let _ = client.destroy_surface(surface);
                let _ = display::close(events);
                return Err(message);
            }
        };
        let (width, height) = first.size;
        // Best effort: `xuid` registered the surface with `inputd` before it
        // answered `CreateSurface`, so the session can be opened right away.
        let input = input::Session::open(surface).ok();
        Ok(ClientWindow {
            events,
            surface,
            buffer: first.buffer,
            va: first.va,
            size: width as u64 * height as u64 * 4,
            rect: (width as i32, height as i32),
            title: title.to_owned(),
            input,
            pending_configure: first.resized,
        })
    }

    /// Resize the surface to `width` x `height`: allocate and attach the new
    /// buffer **before** closing the old one, so a failed attach leaves the
    /// window drawable with its previous buffer (failure
    /// atomicity). On `EINVAL` the compositor saw a newer size than this
    /// Configure carried; keep the old buffer and wait for the next event.
    pub fn reconfigure(&mut self, client: Client, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("reconfigure: zero size".into());
        }
        let size = width as u64 * height as u64 * 4;
        let (buffer, va, _) = sys::display_create_buffer(size)
            .map_err(|code| format!("create_buffer: errno {code}"))?;
        if let Err(code) = client.attach_buffer(self.surface, buffer, size) {
            // The new buffer is not used; release it and keep the old one.
            let _ = sys::display_close_buffer(buffer);
            return Err(format!("attach_buffer: errno {code}"));
        }
        if self.buffer != 0 {
            let _ = sys::display_close_buffer(self.buffer);
        }
        self.buffer = buffer;
        self.va = va;
        self.size = size;
        self.rect = (width as i32, height as i32);
        Ok(())
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
        if self.buffer != 0 {
            // The compositor holds its own reference to the attached buffer,
            // so this only releases the client's mapping and quota charge.
            let _ = sys::display_close_buffer(self.buffer);
        }
    }
}

/// Why [`attach_new_buffer`] failed.
enum AttachError {
    /// The compositor answered `EINVAL`: it no longer has a surface of this
    /// size (a maximize or resize landed before the attach), or it has gone.
    Refused,
    /// Anything else (no buffer quota, a dead compositor): not retryable.
    Failed(String),
}

/// Allocate a shared buffer, attach it to `surface`, return its handle and
/// mapping. A failed attach closes the buffer again so it does not count
/// against the per-process quota until task exit.
fn attach_new_buffer(client: Client, surface: u64, size: u64) -> Result<(u64, u64), AttachError> {
    let (buffer, va, _) = sys::display_create_buffer(size)
        .map_err(|code| AttachError::Failed(format!("create_buffer: errno {code}")))?;
    if let Err(code) = client.attach_buffer(surface, buffer, size) {
        let _ = sys::display_close_buffer(buffer);
        return Err(if code == -errno::EINVAL {
            AttachError::Refused
        } else {
            AttachError::Failed(format!("attach_buffer: errno {code}"))
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
}

/// Attach the window's first buffer. The compositor may resize the new
/// surface (maximize, a resize drag, a size request) between `CreateSurface`
/// and this attach, and then refuses a buffer sized for the old geometry.
/// `Configure` carries the surface's current size, so follow it and retry,
/// the same recovery [`ClientWindow::reconfigure`] does for a live window.
fn attach_first_buffer(
    client: Client,
    events: u64,
    surface: u64,
    mut width: u32,
    mut height: u32,
) -> Result<FirstBuffer, String> {
    let mut resized = None;
    for _ in 0..ATTACH_RETRIES {
        let size = width as u64 * height as u64 * 4;
        match attach_new_buffer(client, surface, size) {
            Ok((buffer, va)) => {
                return Ok(FirstBuffer {
                    buffer,
                    va,
                    size: (width, height),
                    resized,
                })
            }
            Err(AttachError::Failed(message)) => return Err(message),
            Err(AttachError::Refused) => match newest_configure(events)? {
                Some((w, h)) if w > 0 && h > 0 => {
                    (width, height) = (w as u32, h as u32);
                    resized = Some((w, h));
                }
                _ => break,
            },
        }
    }
    Err(format!("attach_buffer: errno {}", -errno::EINVAL))
}

/// The newest `Configure` queued on `events`, waiting briefly for the first
/// one. Other events are dropped (nothing has been drawn yet, so pointer and
/// key events carry no meaning), but a close request is an error: the window
/// is already gone.
fn newest_configure(events: u64) -> Result<Option<(i32, i32)>, String> {
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
                    return Err("window closed while opening".into());
                }
                if let Some(Event::Configure { width, height, .. }) = display::decode_event(&parcel)
                {
                    newest = Some((width, height));
                }
                // Anything still queued is already here; do not wait again.
                wait = 0;
            }
            Err(code) if code == -errno::ETIMEDOUT => return Ok(newest),
            Err(code) => return Err(format!("event drain: errno {code}")),
        }
    }
}

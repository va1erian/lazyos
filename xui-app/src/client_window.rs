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

use crate::display::{self, Client};
use crate::input;
use crate::sys;

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
        let size = width as u64 * height as u64 * 4;
        let (buffer, va) = match attach_new_buffer(client, surface, size) {
            Ok(pair) => pair,
            Err(message) => {
                let _ = client.destroy_surface(surface);
                let _ = display::close(events);
                return Err(message);
            }
        };
        // Best effort: `xuid` registered the surface with `inputd` before it
        // answered `CreateSurface`, so the session can be opened right away.
        let input = input::Session::open(surface).ok();
        Ok(ClientWindow {
            events,
            surface,
            buffer,
            va,
            size,
            rect: (width as i32, height as i32),
            title: title.to_owned(),
            input,
        })
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

/// Allocate a shared buffer, attach it to `surface`, return its handle and
/// mapping. A failed attach closes the buffer again so it does not count
/// against the per-process quota until task exit.
fn attach_new_buffer(client: Client, surface: u64, size: u64) -> Result<(u64, u64), String> {
    let (buffer, va, _) =
        sys::display_create_buffer(size).map_err(|code| format!("create_buffer: errno {code}"))?;
    if let Err(code) = client.attach_buffer(surface, buffer, size) {
        let _ = sys::display_close_buffer(buffer);
        return Err(format!("attach_buffer: errno {code}"));
    }
    Ok((buffer, va))
}

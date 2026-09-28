//! The compositor-side state of a client-mode window (`xuid` surface).
//!
//! Split out of `backend.rs` so the surface lifecycle (create, attach,
//! destroy, and the event channel that goes with each surface) lives in one
//! small unit with a single owner of every handle it creates.

use crate::display::{self, Client};
use crate::sys;

/// The live compositor connection of a client-mode backend.
pub struct ClientState {
    /// The compositor endpoint (`Client::connect`).
    pub client: Client,
    /// This task's end of the current surface's event channel (0 when none).
    pub events: u64,
    /// The surface id from `CreateSurface` (0 when no window is open).
    pub surface: u64,
    /// The shared pixel buffer mapping.
    pub va: u64,
    pub size: u64,
    /// The surface size in pixels.
    pub rect: (i32, i32),
}

impl ClientState {
    /// A connection with no window yet.
    pub fn new(client: Client) -> ClientState {
        ClientState {
            client,
            events: 0,
            surface: 0,
            va: 0,
            size: 0,
            rect: (0, 0),
        }
    }

    /// Create the window's surface, its pixel buffer and its event channel.
    ///
    /// Handle transfers MOVE the handle (`kernel/src/ipc/channels.rs`), so
    /// each surface gets a fresh event pair: reusing one peer for a second
    /// `CreateSurface` would send a handle this task no longer owns. Every
    /// failure path releases what was created so far, so no half-built
    /// surface stays in the compositor with nothing able to destroy it.
    pub fn open_surface(&mut self, width: u32, height: u32, title: &str) -> Result<(), String> {
        if self.surface != 0 {
            return Err("client mode supports one open window at a time".to_string());
        }
        let (events, peer) =
            sys::msg_create_pair().map_err(|code| format!("event pair: errno {code}"))?;
        let surface = match self
            .client
            .create_surface(width as u64, height as u64, title, peer)
        {
            Ok(surface) => surface,
            Err(code) => {
                // The peer may or may not have been consumed; closing a
                // stale handle only fails harmlessly.
                let _ = display::close(peer);
                let _ = display::close(events);
                return Err(format!("create_surface: errno {code}"));
            }
        };
        let size = width as u64 * height as u64 * 4;
        let va = match self.attach_new_buffer(surface, size) {
            Ok(va) => va,
            Err(message) => {
                let _ = self.client.destroy_surface(surface);
                let _ = display::close(events);
                return Err(message);
            }
        };
        self.events = events;
        self.surface = surface;
        self.va = va;
        self.size = size;
        self.rect = (width as i32, height as i32);
        Ok(())
    }

    /// Allocate a shared buffer, attach it to `surface`, return its mapping.
    fn attach_new_buffer(&self, surface: u64, size: u64) -> Result<u64, String> {
        let (buffer, va, _) = sys::display_create_buffer(size)
            .map_err(|code| format!("create_buffer: errno {code}"))?;
        self.client
            .attach_buffer(surface, buffer, size)
            .map_err(|code| format!("attach_buffer: errno {code}"))?;
        Ok(va)
    }

    /// Destroy the surface and close its event channel, if a window is open.
    pub fn close_surface(&mut self) {
        if self.surface != 0 {
            let _ = self.client.destroy_surface(self.surface);
            self.surface = 0;
        }
        if self.events != 0 {
            let _ = display::close(self.events);
            self.events = 0;
        }
        self.va = 0;
        self.size = 0;
    }
}

//! The game's `xuid` window: raw frames out, key records in.
//!
//! The window is an ordinary client-mode surface (`xui_app::client_window`):
//! two shared buffer slots, a one-way `Present` per frame, and the
//! compositor's `BufferRelease` pacing which slot is free. When neither
//! slot is free the frame is dropped, never waited for: the engine keeps
//! its own clock. Keys arrive on the `inputd` session when the service
//! runs, else as the compositor's legacy `KeyDown`/`KeyUp`; every edge
//! becomes one `Key` record ([`KeyRecord`]) on the protocol's input stream,
//! in the Quake keynum the layout types ([`keymap`]), releases included —
//! and a focus lost becomes releases plus `ClearKeys`, exactly what
//! `keys.c`'s `ClearAllStates` is for. A maximized game holds the keyboard
//! grab (`window_input.rs` / [`session`]).

use xui_app::client_window::{ClientWindow, SurfaceRole};
use xui_app::display::{self, Client, Event, FrameEvent};
pub use xui_app::client_window::OpenError;
use xui_app::input::{self, KeyStatePage};
use xui_app::sys;
use xui_core::Rect;

use crate::lazy::pixels;
use crate::lazy::session::SessionKeys;
use crate::lazy::window_input::PumpEvent;

/// Receive buffer for one event; events are far smaller.
const EVENT_BYTES: usize = 4096;
/// The smallest content size: the engine's smallest mode.
const MIN_W: u32 = 320;
const MIN_H: u32 = 200;
/// The window a desktop session opens: a 4:3 box, WinQuake's monitor. A
/// maximized window on a phone-sized display still leaves the status bar
/// readable at the engine's upgraded sizes.
pub const WIDTH: usize = 960;
pub const HEIGHT: usize = 720;

/// A key edge as a protocol record: the Quake keynum (`keys.rs`'s `K_*`
/// table, printable = ASCII) and the character the layout typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRecord {
    pub keynum: u8,
    pub down: bool,
    pub ch: u32,
}

/// The window: the surface, the key plumbing, and the RGBA image the next
/// present copies from.
pub struct Window {
    client: Client,
    pub(crate) window: ClientWindow,
    image: Vec<u8>,
    pub(crate) session_keys: SessionKeys,
    /// The page `inputd` keeps the held keys in while the window is
    /// focused.
    pub(crate) key_page: Option<KeyStatePage>,
    pub(crate) grab: crate::lazy::session::GrabPolicy,
    /// The user closed the window.
    pub(crate) closed: bool,
}

impl Window {
    /// Connect to `xuid` and open a `width` x `height` window.
    pub fn open(width: usize, height: usize, title: &str) -> Result<Window, OpenError> {
        let client = Client::connect()
            .map_err(|code| OpenError::Failed(format!("connect: errno {code}")))?;
        let mut window =
            ClientWindow::open(client, width as u32, height as u32, title, SurfaceRole::Window)?;
        let (w, h) = window.rect;
        let pending = std::mem::take(&mut window.pending_events);
        let configure = window.pending_configure.take();
        let mut this = Window {
            client,
            window,
            image: vec![0; w as usize * h as usize * 4],
            session_keys: SessionKeys::new(),
            key_page: None,
            grab: crate::lazy::session::GrabPolicy::new(),
            closed: false,
        };
        this.attach_key_page();
        // Resizable down to the engine's smallest 320x200, so the window
        // can be maximized (a maximized game holds the keyboard grab,
        // `window_input.rs`). An older compositor refuses it and the
        // window stays fixed-size.
        let surface = this.window.surface;
        let _ = client.set_size_hints(surface, MIN_W, MIN_H, 0, 0);
        if let Some((w, h)) = configure {
            this.resize(w, h);
        }
        for event in pending {
            let mut out = Vec::new();
            this.route(event, &mut out);
        }
        Ok(this)
    }

    /// Drain both event channels without blocking: slot releases, keys,
    /// resizes. `out` collects the `KeyRecord`s produced this pump; the
    /// caller sends them to the engine. The return says whether the window
    /// has been closed.
    pub fn drain(&mut self) -> (Vec<PumpEvent>, bool) {
        let mut out = Vec::new();
        let mut buf = vec![0u8; EVENT_BYTES];
        while let Some(parcel) = next_message(self.window.events, &mut buf) {
            if parcel.header.method == display::METHOD_WINDOW_CLOSE {
                self.closed = true;
                return (out, true);
            }
            if let Some(frame) = display::decode_frame_event(&parcel) {
                match frame {
                    FrameEvent::BufferRelease { slot } => {
                        self.window.slots.released(slot);
                    }
                    FrameEvent::FrameDone { seq } => {
                        self.window.slots.frame_done(seq);
                    }
                }
            } else if let Some(event) = display::decode_event(&parcel) {
                self.route(event, &mut out);
            }
        }
        if let Some(session) = self.window.input {
            while let Some(parcel) = next_message(session.events, &mut buf) {
                if let Some(event) = input::decode_event(&parcel) {
                    self.session_event(session, event, &mut out);
                }
            }
        }
        self.reconcile_keys(&mut out);
        (out, self.closed)
    }

    /// One compositor event: legacy keys and size changes (pointer input is
    /// not used: v1 is keyboard-only, id's 1996 controls).
    fn route(&mut self, event: Event, out: &mut Vec<PumpEvent>) {
        match event {
            Event::KeyDown { key } => {
                if let Some(edge) = crate::lazy::keymap::from_legacy(key) {
                    out.push(PumpEvent::Key(KeyRecord {
                        keynum: edge.0,
                        down: true,
                        ch: edge.1,
                    }));
                }
            }
            Event::KeyUp { key } => {
                if let Some(edge) = crate::lazy::keymap::from_legacy(key) {
                    out.push(PumpEvent::Key(KeyRecord {
                        keynum: edge.0,
                        down: false,
                        ch: edge.1,
                    }));
                }
            }
            Event::Configure {
                width,
                height,
                state,
            } => {
                self.resize(width, height);
                self.maximized_configured(state);
            }
            _ => {}
        }
    }

    /// Follow a `Configure`: the slots reallocate as each is next drawn
    /// into.
    pub(crate) fn resize(&mut self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        self.window.resize(width, height);
        self.image = vec![0; width as usize * height as usize * 4];
    }

    /// Scale the frame into the window and present it, if a buffer slot is
    /// free. `ratio` is the picture's display aspect (the 4:3 box, or the
    /// window's own aspect for a native picture).
    pub fn draw(&mut self, pixels: &[u8], w: usize, h: usize, ratio: f64) {
        if self.closed {
            return;
        }
        let (win_w, win_h) = self.window.rect;
        pixels::blit(
            pixels,
            w,
            h,
            &mut self.image,
            win_w as usize,
            win_h as usize,
            ratio,
        );
        let surface = self.window.surface;
        let slots = &mut self.window.slots;
        let Ok(Some(slot)) = slots.acquire(self.client, surface, win_w, win_h) else {
            return; // both slots on screen or a resize in flight: drop the frame
        };
        let full = Rect::new(0, 0, win_w, win_h);
        slots.damage(full);
        if !slots.sync(slot, &self.image) {
            return;
        }
        let Some(seq) = slots.submit(slot) else {
            return;
        };
        if self.client.present(surface, slot, seq, full).is_err() {
            // The compositor never saw it, so it will never release the slot.
            slots.cancel(slot, seq);
        }
    }

    pub fn set_title(&mut self, title: &str) {
        if self.window.title != title && self.client.set_title(self.window.surface, title).is_ok() {
            self.window.title = title.to_owned();
        }
    }

    /// Tear the surface down (the game is quitting).
    pub fn close(&mut self) {
        self.window.close(self.client);
    }

    pub fn content_size(&self) -> (usize, usize) {
        let (w, h) = self.window.rect;
        (w as usize, h as usize)
    }
}

/// The next queued message on `channel`, decoded, or `None` when nothing is
/// queued (or the peer is gone).
fn next_message(channel: u64, buf: &mut [u8]) -> Option<libmessenger::Parcel> {
    if !matches!(sys::msg_queued(channel), Ok(queued) if queued > 0) {
        return None;
    }
    loop {
        match sys::msg_recv(channel, buf, sys::EXPIRED_DEADLINE) {
            Ok(result) => {
                if let Some(parcel) = display::decode_message(&buf[..result.bytes as usize]) {
                    return Some(parcel);
                }
            }
            Err(code) if code == -sys::errno::ETIMEDOUT => return None,
            Err(_) => return None,
        }
    }
}

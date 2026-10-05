//! The game's `xuid` window: raw pixels out, keys in.
//!
//! The window is an ordinary client-mode surface (`xui_app::client_window`):
//! two shared buffer slots, a one-way `Present` per frame, and the compositor's
//! `BufferRelease` pacing which slot is free. When neither slot is free the
//! frame is dropped, never waited for: the engine keeps its own 35 Hz clock.
//! Keys arrive on the `inputd` session when the service runs, else as the
//! compositor's legacy `KeyDown`/`KeyUp`; both feed one edge-tracking queue.

use lazydoom::keymap;
use lazydoom::keys::Keys;
use lazydoom::pixels;
use xui_app::client_window::{ClientWindow, SurfaceRole};
use xui_app::display::{self, Client, Event, FrameEvent};
use xui_app::input::{self, Event as InputEvent, KeyState};
use xui_app::sys::{self, errno};
use xui_core::Rect;

/// Receive buffer for one event; events are far smaller.
const EVENT_BYTES: usize = 4096;

pub struct Window {
    client: Client,
    window: ClientWindow,
    /// The window-sized RGBA image the next present copies from.
    image: Vec<u8>,
    pub keys: Keys,
}

impl Window {
    /// Connect to `xuid` and open a `width` x `height` window.
    pub fn open(width: usize, height: usize, title: &str) -> Result<Window, String> {
        let client = Client::connect().map_err(|code| format!("connect: errno {code}"))?;
        let mut window = ClientWindow::open(
            client,
            width as u32,
            height as u32,
            title,
            SurfaceRole::Window,
        )
        .map_err(|e| e.to_string())?;
        let (w, h) = window.rect;
        let pending = std::mem::take(&mut window.pending_events);
        let configure = window.pending_configure.take();
        let mut this = Window {
            client,
            window,
            image: vec![0; w as usize * h as usize * 4],
            keys: Keys::new(),
        };
        if let Some((w, h)) = configure {
            this.resize(w, h);
        }
        for event in pending {
            this.route(event);
        }
        Ok(this)
    }

    /// Scale the engine's frame into the window and present it, if a buffer
    /// slot is free.
    pub fn draw(&mut self, frame: &[u32], frame_w: usize, frame_h: usize) {
        self.pump();
        let (w, h) = self.window.rect;
        pixels::blit(
            frame,
            frame_w,
            frame_h,
            &mut self.image,
            w as usize,
            h as usize,
        );
        let surface = self.window.surface;
        let slots = &mut self.window.slots;
        let Ok(Some(slot)) = slots.acquire(self.client, surface, w, h) else {
            return; // both slots on screen or a resize in flight: drop the frame
        };
        let full = Rect::new(0, 0, w, h);
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

    /// Drain both event channels without blocking: only queued messages are
    /// read (the channel counters say how many), so an idle pump costs two
    /// cheap syscalls and never parks the game for a timer tick.
    pub fn pump(&mut self) {
        let mut buf = vec![0u8; EVENT_BYTES];
        while let Some(parcel) = next_message(self.window.events, &mut buf) {
            if parcel.header.method == display::METHOD_WINDOW_CLOSE {
                self.quit();
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
                self.route(event);
            }
        }
        let Some(session) = self.window.input else {
            return;
        };
        while let Some(parcel) = next_message(session.events, &mut buf) {
            match input::decode_event(&parcel) {
                Some(InputEvent::Key {
                    code, sym, state, ..
                }) => {
                    let Some(key) = keymap::from_session(code, sym) else {
                        continue;
                    };
                    match state {
                        KeyState::Down => self.keys.press(key),
                        KeyState::Up => self.keys.release(key),
                        KeyState::Repeat => {}
                    }
                }
                // Focus left: nothing may stay held, or the player runs on.
                Some(InputEvent::Leave) => self.keys.release_all(),
                _ => {}
            }
        }
    }

    /// One compositor event: legacy keys and size changes (pointer input is
    /// not used: v1 is keyboard-only).
    fn route(&mut self, event: Event) {
        match event {
            Event::KeyDown { key } => {
                if let Some(key) = keymap::from_legacy(key) {
                    self.keys.press(key);
                }
            }
            Event::KeyUp { key } => {
                if let Some(key) = keymap::from_legacy(key) {
                    self.keys.release(key);
                }
            }
            Event::Configure { width, height, .. } => self.resize(width, height),
            _ => {}
        }
    }

    /// Follow a `Configure`: the slots reallocate as each is next drawn into.
    fn resize(&mut self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        self.window.resize(width, height);
        self.image = vec![0; width as usize * height as usize * 4];
    }

    /// The user closed the window: tear the surface down and end the game.
    fn quit(&mut self) -> ! {
        self.window.close(self.client);
        println!("DOOM:QUIT:PASS");
        std::process::exit(0);
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
            Err(code) if code == -errno::ETIMEDOUT => return None,
            Err(_) => return None,
        }
    }
}

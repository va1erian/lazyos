//! Shared display-client plumbing for `dragdemo` (issue #194 split): the [`App`]
//! handle, surface attachment, and the retry/park/fatal helpers every role uses.
//!
//! Split out of `dragdemo.rs`, which was past the file-size budget; the code is
//! unchanged.

use alloc::vec;
use alloc::vec::Vec;
use user::messenger::display::{Canvas, Client, Rect};
use user::messenger::{self, clipboard, errno, Endpoint, Error};
use user::sys;

use super::{CONNECT_ATTEMPTS, H, W};

/// A connected display client with its surface, event endpoint and pixel
/// buffer.
pub(super) struct App {
    pub(super) display: Client,
    pub(super) surface: u64,
    pub(super) events: Endpoint,
    canvas: Canvas,
    pub(super) buffer: Vec<u8>,
}

/// Create the surface, allocate and attach its buffer, and commit the first
/// frame.
pub(super) fn attach(display: Client, title: &str, draw: &dyn Fn(&mut Canvas)) -> App {
    let (events, events_server) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(_) => fatal("create_pair"),
    };
    let surface = match display.create_surface(W as u64, H as u64, title, &events_server) {
        Ok(surface) => surface,
        Err(_) => fatal("create_surface"),
    };
    let bytes = (W * H * 4) as u64;
    let (buffer, va, _) = match sys::display_create_buffer(bytes) {
        Ok(pair) => pair,
        Err(_) => fatal("create_buffer"),
    };
    // Safety: `va` is the mapping of the buffer just created, `W * H * 4`
    // bytes long.
    let mut canvas = unsafe { Canvas::new(va, W, H) };
    draw(&mut canvas);
    if display.attach_buffer(surface, buffer, bytes).is_err() {
        fatal("attach_buffer");
    }
    if display.commit(surface, Rect::new(0, 0, W, H)).is_err() {
        fatal("commit");
    }
    App {
        display,
        surface,
        events,
        canvas,
        buffer: vec![0u8; 4096],
    }
}

impl App {
    /// Repaint the whole surface from `draw` and commit it.
    pub(super) fn redraw(&mut self, draw: &dyn Fn(&mut Canvas)) {
        draw(&mut self.canvas);
        let _ = self.display.commit(self.surface, Rect::new(0, 0, W, H));
    }
}

/// Connect to the compositor, retrying while it binds and registers.
pub(super) fn connect_display() -> Client {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(client) = Client::connect() {
            return client;
        }
        park_tick();
    }
    fatal("no compositor")
}

/// Resolve `clipboardd`, retrying while the supervisor starts it. `None` when
/// it never appears (the plain `LAZYOS_XUID=1` demo boots without services).
pub(super) fn connect_clipboard() -> Option<clipboard::Client> {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(client) = clipboard::Client::connect() {
            return Some(client);
        }
        park_tick();
    }
    None
}

/// Whether an error is the `recv` deadline firing.
pub(super) fn is_timeout(error: Error) -> bool {
    matches!(error, Error::Errno(code) if code == -errno::ETIMEDOUT)
}

/// Sleep one PIT tick ([`sys::nap`]).
pub(super) fn park_tick() {
    sys::nap();
}

/// Report a fatal setup failure on serial, then exit non-zero.
pub(super) fn fatal(what: &str) -> ! {
    sys::write_str("dragdemo: fatal: ");
    sys::write_str(what);
    sys::write_str("\n");
    sys::exit(1)
}

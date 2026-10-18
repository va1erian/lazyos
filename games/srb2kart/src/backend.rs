//! What the engine's platform layer needs from the host, as a trait: the
//! plug point between the C seam (`hooks.rs`) and the program's two ways of
//! running, [`crate::headless::Headless`] (no display, checksummed frames, a
//! virtual clock) and the desktop window (`window.rs`, K1).
//!
//! The engine is single-threaded and calls every hook from the thread that
//! runs it, so a backend needs no locking.

/// One input event for the engine (`struct lz_event` in `lazyos_host.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// [`Event::KEY_DOWN`], [`Event::KEY_UP`] or [`Event::CLOSE`].
    pub kind: i32,
    /// An SRB2 `keys.h` code (`KEY_*`, ASCII); 0 for [`Event::CLOSE`].
    pub key: i32,
}

impl Event {
    pub const KEY_DOWN: i32 = 1;
    pub const KEY_UP: i32 = 2;
    pub const CLOSE: i32 = 3;
}

/// The engine's 8-bit frame, borrowed for one `present` call.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    /// `pitch * height` palette indices, rows top to bottom.
    pub pixels: &'a [u8],
    pub width: usize,
    pub height: usize,
    /// Bytes per row, at least `width`.
    pub pitch: usize,
    /// `r, g, b` for each of the 256 indices.
    pub palette: &'a [u8; 768],
}

/// What a frame's receiver does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The run is over with this process exit status (headless, once the
    /// requested number of frames is in).
    Exit(i32),
}

pub trait Backend {
    /// Open (or resize) the output to `width` x `height`; `Err` carries the
    /// reason, which the engine reports as a fatal error.
    fn open(&mut self, width: usize, height: usize, title: &str) -> Result<(), String>;
    /// Show `frame`.
    fn present(&mut self, frame: &Frame<'_>) -> Flow;
    /// The oldest pending input event.
    fn poll(&mut self) -> Option<Event>;
    /// Monotonic nanoseconds since start-up.
    fn ticks_ns(&mut self) -> u64;
    /// Wait about `us` microseconds.
    fn sleep(&mut self, us: u32);
    /// Start-up is over: the engine's game loop is drawing.
    fn ready(&mut self) {}
    /// The engine is leaving: tear the output down (the window must be
    /// closed explicitly, it does not die with the process).
    fn close(&mut self) {}
}

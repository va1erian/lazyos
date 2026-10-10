//! The record bridge: the whole `quake-srp` program
//! ([`crate::sys::run`]) driven over in-memory pipes, exactly the way the
//! browser page hosts it — only here the "page" is this process:
//!
//! - [`Pump`] (the `Read` side): the key edges the window's pumps
//!   collected, a `Window` record whenever the render box would change,
//!   and one `Tick` per display refresh. The program parks on this
//!   between turns, so the main thread's work is pumping and pacing.
//!   Headless: the harness's `set_resolution` call once and a fixed tick
//!   instead (the deterministic gate the census pins).
//! - [`Sink`] (the `Write` side): the program's records, decoded once per
//!   `flush` (`sys.rs` flushes at every turn's `Sync`): the frame is
//!   scaled onto the window ([`pixels`]) or, headless, checksummed
//!   ([`Headless`]); the `Quit` record ends the program; the sound, the
//!   CD and rumbles are v1 points of silence.
//!
//! The two halves share only the window, behind a `RefCell` on one `Rc`;
//! the engine drains on its reads and draws on its writes, which never
//! borrow both at once, so no borrow ever conflicts. The verdict markers
//! mirror the Doom port's: `QUAKE:UP:PASS mode=window|headless`,
//! `QUAKE:QUIT:PASS`, `QUAKE:FRAME:n:crc` every hundred headless frames,
//! and the headless `QUAKE:HEADLESS:PASS frames=N crc=<hex>` (also in
//! `fhs::state::QUAKE_RESULT`, since the Terminal reports one line and
//! the harness cats the file).

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::time::Instant;

use crate::lazy::headless::Headless;
use crate::lazy::launch::Launch;
use crate::lazy::records;
use crate::lazy::window::{KeyRecord, Window};
use crate::lazy::window_input::PumpEvent;

/// The desktop's tick pacing, the page's display refresh in the browser:
/// one tick at most this often, with the seconds since the last one
/// attached. The engine gates itself at id's 72 fps on top of this
/// (`host_filter_time`), so on a fast machine every refresh runs a frame.
const PACK_DT: f64 = 1.0 / 60.0;
/// A tick never says more than this passed, so a suspended process does
/// not fast-forward the game on its next tick.
const MAX_DT: f64 = 0.25;
/// The headless tick's fixed step: the gate the port's own checks run on
/// (`bench.py` drives the page at 1/72, id's `host_maxfps`), so a headless
/// run's frame N is the same on every machine — real elapsed time between
/// two ticks would make the deterministic verdict worthless.
const FIXED_DT: f64 = 1.0 / 72.0;
/// The headless budget when `-frames` does not name one: 300, the
/// plan's D7 budget (the attract loop otherwise never ends, and the
/// harness must terminate).
pub const HEADLESS_DEFAULT_FRAMES: u32 = 300;

/// The window both halves use.
pub type Shared = Rc<RefCell<WindowSlot>>;

/// The window's only shared piece.
pub struct WindowSlot {
    pub window: Option<Window>,
}

/// The `Read` half: keys, resizes and ticks in.
pub struct Pump {
    shared: Shared,
    /// Whether headless mode is running: its seeds and tick pacing differ.
    headless: bool,
    /// `seq` of the next tick record.
    seq: u32,
    /// The last tick's wall clock; `None` until the first.
    last_tick: Option<Instant>,
    /// Input records not yet handed over.
    inbuf: Vec<u8>,
    /// The window size the engine has been told about.
    sent_window: Option<(u32, u32)>,
}

/// The `Write` half: the records the program produces, one turn at a time.
pub struct Sink {
    shared: Shared,
    /// The last `State` record's native flag, for the next frame's ratio.
    native: bool,
    /// Headless checksums, when running headless (checksummed); `None` in
    /// windowed mode, where the frame goes to the window.
    headless: Option<Headless>,
    /// The engine's writes, up to the next `flush` boundary.
    unwritten: Vec<u8>,
}

/// Build the two halves of one run: `window` for a windowed game,
/// `budget` the headless frame budget ([`HEADLESS_DEFAULT_FRAMES`] when
/// `-frames` did not name one; `None` in windowed mode, where no frame is
/// checksummed).
pub fn connect(_: &Launch, window: Option<Window>, budget: Option<u32>) -> (Pump, Sink) {
    let headless = window.is_none();
    let budget = budget.unwrap_or(HEADLESS_DEFAULT_FRAMES);
    let shared: Shared = Rc::new(RefCell::new(WindowSlot { window }));
    let mut pump = Pump {
        shared: shared.clone(),
        headless,
        seq: 0,
        last_tick: None,
        inbuf: Vec::new(),
        sent_window: None,
    };
    let sink = Sink {
        shared: shared.clone(),
        native: false,
        headless: headless.then(|| Headless::new(budget)),
        unwritten: Vec::new(),
    };
    if headless {
        // The harness's opening call: id's picture (320x200, the 4:3 box),
        // ahead of the ticks.
        pump.inbuf.extend(records::call(1, "set_resolution 320 200"));
    }
    (pump, sink)
}

impl Read for Pump {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let closed = match self.shared.borrow_mut().window.as_mut() {
                Some(window) => {
                    let (pumped, closed) = window.drain();
                    for event in pumped {
                        match event {
                            PumpEvent::Key(KeyRecord { keynum, down, ch }) => {
                                self.inbuf.extend(records::key(keynum, down, ch));
                            }
                            PumpEvent::ClearKeys => self.inbuf.extend(records::clear_keys()),
                        }
                    }
                    closed
                }
                None => false,
            };
            if closed {
                if self.inbuf.is_empty() {
                    // A clean end of the stream: the program quits.
                    return Ok(0);
                }
                // Deliver the queued records first; the next read answer
                // is the end.
                return self.hand_out(buf);
            }
            self.announce_window();
            if self.inbuf.is_empty() && self.tick_due() {
                let tick = self.tick_record();
                self.inbuf.extend(tick);
            }
            if !self.inbuf.is_empty() {
                return self.hand_out(buf);
            }
            // Nothing yet: wait a little and pump again. A windowed game
            // sleeps; a headless one ticks as fast as it can.
            if !self.headless {
                xui_app::sys::sleep_millis(4);
            }
        }
    }
}

impl Pump {
    /// Hand as much of `inbuf` to the caller as the buffer takes.
    fn hand_out(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inbuf.len().min(buf.len());
        buf[..n].copy_from_slice(&self.inbuf[..n]);
        self.inbuf.drain(..n);
        Ok(n)
    }

    /// The engine renders natively only into a box it has been told about:
    /// the `Window` record every time the window's size changes (`vid.rs`'s
    /// `set_window`).
    fn announce_window(&mut self) {
        let size = self
            .shared
            .borrow()
            .window
            .as_ref()
            .map(Window::content_size)
            .map(|(w, h)| (w as u32, h as u32));
        if size.is_some() && self.sent_window != size {
            self.sent_window = size;
            self.inbuf.extend(records::window(size.unwrap().0, size.unwrap().1));
        }
    }

    /// Whether the next display refresh is due (windows), or the next tick
    /// is immediate (headless: the census's fixed step, deterministic).
    fn tick_due(&self) -> bool {
        if self.headless {
            return true;
        }
        match self.last_tick {
            None => true,
            Some(last) => last.elapsed().as_secs_f64() >= PACK_DT,
        }
    }

    /// One tick record: headless says the fixed step ([`FIXED_DT`], the
    /// deterministic census pace); a window says its real elapsed time,
    /// capped ([`MAX_DT`]).
    fn tick_record(&mut self) -> Vec<u8> {
        let now = Instant::now();
        let dt = if self.headless {
            FIXED_DT
        } else {
            match self.last_tick.take() {
                None => 0.0,
                Some(last) => last.elapsed().as_secs_f64().min(MAX_DT),
            }
        };
        self.seq += 1;
        self.last_tick = Some(now);
        records::tick(self.seq, dt)
    }
}

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf(data)
    }

    /// The turn's records (`sys.rs` flushes at each turn's `Sync`): the
    /// kinds the windowed or headless side acts on.
    fn flush(&mut self) -> io::Result<()> {
        self.drained()
    }
}

impl Sink {
    /// Buffer the program's bytes; the boundary at [`flush`] is where a
    /// whole turn's records end.
    fn buf(&mut self, data: &[u8]) -> io::Result<usize> {
        self.unwritten.extend_from_slice(data);
        Ok(data.len())
    }

    /// Split the buffered stream into records and handle each.
    fn drained(&mut self) -> io::Result<()> {
        let bytes = std::mem::take(&mut self.unwritten);
        let mut rest = &bytes[..];
        while rest.len() >= 8 {
            let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
            if rest.len() < 8 + len {
                break;
            }
            let (kind, payload) = (rest[0], &rest[8..8 + len]);
            rest = &rest[8 + len..];
            match kind {
                records::OUT_STATE => {
                    let flags = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    // STATE_NATIVE: the picture is the window's own box
                    // (`proto.rs`; the value is covered by the engine's own
                    // `State` tests).
                    self.native = flags & STATE_NATIVE != 0;
                }
                records::OUT_FRAME => {
                    let w = u16::from_le_bytes(payload[0..2].try_into().unwrap()) as usize;
                    let h = u16::from_le_bytes(payload[2..4].try_into().unwrap()) as usize;
                    let format = payload[4];
                    // FORMAT_INDEXED8: the palette (256 x RGBA) rides
                    // ahead; anything else is RGBA with none (the platform
                    // never asks for indexed frames).
                    const FORMAT_INDEXED8: u8 = 1;
                    let palette = if format == FORMAT_INDEXED8 { 256 * 4 } else { 0 };
                    let pixels = &payload[8 + palette..];
                    match self.headless.as_mut() {
                        Some(headless) => headless.draw(w, h, pixels),
                        None => self.draw_frame(w, h, pixels),
                    }
                }
                // The game's last word (`Sys_Quit`): [`run`] prints the
                // marker and tears the window down after `sys::run`
                // returns.
                records::OUT_QUIT => {}
                // Sync (pacing comes from the tick clock here), Pcm, Audio,
                // Cd, Rumble: v1 is silent and has no pad.
                _ => {}
            }
        }
        Ok(())
    }

    /// One finished frame onto the window.
    fn draw_frame(&mut self, w: usize, h: usize, pixels: &[u8]) {
        let mut slot = self.shared.borrow_mut();
        let Some(window) = slot.window.as_mut() else { return };
        // The picture's aspect: a mode is shown 4:3 (as a 1996 monitor
        // filled its CRT), a native picture is the window's own.
        let (win_w, win_h) = window.content_size();
        let ratio = if self.native {
            (win_w.max(1) as f64) / win_h.max(1) as f64
        } else {
            crate::lazy::pixels::DISPLAY_ASPECT
        };
        window.draw(pixels, w, h, ratio);
    }
}

/// `proto.rs`'s `STATE_NATIVE` bit; the constant is not public, so the
/// platform side keeps the number and the engine's own `State` tests keep
/// it honest.
const STATE_NATIVE: u32 = 32;

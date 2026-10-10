//! Headless mode: no display, every frame checksummed — the Doom port's
//! recipe, on id's deterministic census picture.
//!
//! `quake.elf -headless -frames 200` runs the engine as fast as it can on
//! the harness's fixed ticks (1/72 s, the demo playback's own cadence), so
//! frame N is the same on every machine: the printed
//! `QUAKE:HEADLESS:PASS frames=N crc=<hex>` is a regression check over the
//! whole stack (the engine, the record bridge, the Linux ABI shim) — and
//! the run is a CPU and `read` soak, like the Doom one.
//!
//! Without `-frames` the budget is [`HEADLESS_DEFAULT_FRAMES`]: the
//! attract loop never ends on its own, and the harness must terminate.
//!
//! The verdict line also goes to [`RESULT_FILE`], which this process
//! opens itself: the desktop Terminal reports only the first output line
//! of a command and output the shell redirected has not been reliable for
//! Linux programs, so a harness `cat`s the file instead.

use crate::lazy::crc;

/// The engine writes the frame as RGBA, 4 bytes a pixel — the `Frame`
/// record's body after its palette (see [`crate::lazy::bridge`]).
pub const RGBA_BYTES: usize = 4;

/// Where the verdict line is written (removed at start-up, so a stale one
/// never passes a check).
pub const RESULT_FILE: &str = fhs::state::QUAKE_RESULT;

pub struct Headless {
    /// The frame budget, when `-frames` named one.
    budget: u32,
    /// The records to treat as the harness's opening call — written once,
    /// before the first tick.
    seeded: bool,
    frames: u32,
    last: u32,
}

impl Headless {
    pub fn new(budget: u32) -> Headless {
        let _ = std::fs::remove_file(RESULT_FILE);
        Headless {
            budget,
            seeded: true,
            frames: 0,
            last: 0,
        }
    }

    /// The first read's preamble is pending ([`crate::bridge`]): once.
    pub fn seed_pending(&mut self) -> bool {
        if self.seeded {
            self.seeded = false;
            true
        } else {
            false
        }
    }

    /// One finished frame: checksummed, the verdict at the budget.
    pub fn draw(&mut self, w: usize, h: usize, pixels: &[u8]) {
        if pixels.len() != w * h * RGBA_BYTES {
            return;
        }
        self.frames += 1;
        self.last = crc::frame(pixels);
        if self.frames % 100 == 0 {
            println!("QUAKE:FRAME:{}:{:08x}", self.frames, self.last);
        }
        if self.frames >= self.budget {
            self.finish();
        }
    }

    /// The verdict, in the terminal and the result file, then quit: the
    /// same shape as the Doom headless pass.
    fn finish(&self) -> ! {
        let verdict = format!(
            "QUAKE:HEADLESS:PASS frames={} crc={:08x}",
            self.frames, self.last
        );
        println!("{verdict}");
        let _ = std::fs::write(RESULT_FILE, format!("{verdict}\n"));
        std::process::exit(0);
    }
}

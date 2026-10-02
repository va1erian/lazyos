//! Headless mode: no display, every frame checksummed.
//!
//! `doom.elf -headless -frames N -timedemo demo1` renders the demo as fast as
//! it can (each tic is one frame), so frame `N` is the same on every machine:
//! the printed `DOOM:HEADLESS:PASS frames=N crc=<hex>` is a regression check
//! of the engine on the Linux ABI shim, and the run is a CPU and `read` soak.
//! Without `-frames` the engine runs until it exits on its own (a timedemo
//! ends with its "timed N gametics" line).
//!
//! The verdict line is also written to [`RESULT_FILE`], which this process
//! opens itself: the desktop Terminal reports only the first output line of a
//! command, and output redirected by the shell has not been reliable for
//! Linux programs, so a harness `cat`s the file instead.

use lazydoom::crc;

/// Where the verdict line is written (removed at start-up, so a stale one
/// never passes a check).
pub const RESULT_FILE: &str = "/tmp/doom-result.txt";

pub struct Headless {
    frames: u32,
    limit: Option<u32>,
    last: u32,
}

impl Headless {
    pub fn new(limit: Option<u32>) -> Headless {
        let _ = std::fs::remove_file(RESULT_FILE);
        Headless {
            frames: 0,
            limit,
            last: 0,
        }
    }

    pub fn draw(&mut self, pixels: &[u32]) {
        self.frames += 1;
        self.last = crc::frame(pixels);
        if self.frames % 100 == 0 {
            println!("DOOM:FRAME:{}:{:08x}", self.frames, self.last);
        }
        if self.limit == Some(self.frames) {
            let verdict = format!("DOOM:HEADLESS:PASS frames={} crc={:08x}", self.frames, self.last);
            println!("{verdict}");
            let _ = std::fs::write(RESULT_FILE, format!("{verdict}
"));
            std::process::exit(0);
        }
    }
}

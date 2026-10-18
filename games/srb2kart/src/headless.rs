//! Headless mode: no display, every frame checksummed, a virtual clock.
//!
//! `srb2kart.elf -headless -frames N` runs the engine as fast as it will go
//! (the sleeps cost nothing) and, after frame `N`, prints
//! `SRB2KART:HEADLESS:PASS frames=N crc=<hex>`: a regression check of the
//! engine on the Linux ABI shim and a CPU and `read` soak.
//!
//! The engine paces itself by the clock, so a real clock would make the number
//! of tics before frame `N` depend on how fast the machine is. The clock here
//! is virtual: it advances by [`TICK_NS`] on every query and by exactly the
//! time of every sleep, so the same engine makes the same frames on every run
//! and every machine, and the checksum is a check of the engine, not of the
//! host.
//!
//! The verdict line is also written to [`RESULT_FILE`], which this process
//! opens itself: the desktop Terminal reports only the first output line of a
//! command, so a harness `cat`s the file instead.

use crate::backend::{Backend, Event, Flow, Frame};
use crate::crc;
use crate::launch::RESULT_FILE;

/// How far the virtual clock moves per query. The engine busy-waits on the
/// clock between frames, so this only sets how many queries that takes.
pub const TICK_NS: u64 = 20_000;

pub struct Headless {
    limit: Option<u32>,
    frames: u32,
    last: u32,
    now_ns: u64,
}

impl Headless {
    pub fn new(limit: Option<u32>) -> Headless {
        Headless {
            limit,
            frames: 0,
            last: 0,
            now_ns: 0,
        }
    }

    /// The verdict line for `frames` frames ending in checksum `crc`.
    pub fn verdict(frames: u32, crc: u32) -> String {
        format!("SRB2KART:HEADLESS:PASS frames={frames} crc={crc:08x}")
    }
}

/// `frame` as a binary PPM (palette applied, row padding dropped).
pub fn ppm(frame: &Frame<'_>) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    for row in frame.pixels.chunks(frame.pitch).take(frame.height) {
        for &index in &row[..frame.width] {
            let at = usize::from(index) * 3;
            out.extend_from_slice(&frame.palette[at..at + 3]);
        }
    }
    out
}

impl Backend for Headless {
    fn open(&mut self, _width: usize, _height: usize, _title: &str) -> Result<(), String> {
        Ok(())
    }

    fn present(&mut self, frame: &Frame<'_>) -> Flow {
        let Some(crc) = crc::frame(
            frame.pixels,
            frame.width,
            frame.height,
            frame.pitch,
            frame.palette,
        ) else {
            return Flow::Continue;
        };
        self.frames += 1;
        self.last = crc;
        if self.frames % 100 == 0 {
            println!("SRB2KART:FRAME:{}:{crc:08x}", self.frames);
        }
        if self.limit == Some(self.frames) {
            let verdict = Self::verdict(self.frames, crc);
            println!("{verdict}");
            let _ = std::fs::write(RESULT_FILE, format!("{verdict}\n"));
            // A look at the last frame for whoever runs this by hand:
            // `SRB2KART_DUMP=/tmp/frame.ppm` writes it as a binary PPM.
            if let Some(path) = std::env::var_os("SRB2KART_DUMP") {
                let _ = std::fs::write(path, ppm(frame));
            }
            return Flow::Exit(0);
        }
        Flow::Continue
    }

    fn poll(&mut self) -> Option<Event> {
        None
    }

    fn ticks_ns(&mut self) -> u64 {
        self.now_ns += TICK_NS;
        self.now_ns
    }

    fn sleep(&mut self, us: u32) {
        self.now_ns += u64::from(us) * 1000;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: [u8; 768] = [7; 768];

    fn frame(pixels: &[u8]) -> Frame<'_> {
        Frame {
            pixels,
            width: 2,
            height: 2,
            pitch: 2,
            palette: &PALETTE,
        }
    }

    #[test]
    fn the_clock_is_virtual_and_deterministic() {
        let mut a = Headless::new(None);
        let mut b = Headless::new(None);
        assert_eq!(a.ticks_ns(), TICK_NS);
        a.sleep(1000);
        assert_eq!(a.ticks_ns(), 2 * TICK_NS + 1_000_000);
        b.ticks_ns();
        b.sleep(1000);
        assert_eq!(b.ticks_ns(), 2 * TICK_NS + 1_000_000);
    }

    #[test]
    fn it_exits_after_the_requested_frames() {
        let mut headless = Headless::new(Some(3));
        let pixels = [0u8, 1, 2, 3];
        assert_eq!(headless.present(&frame(&pixels)), Flow::Continue);
        assert_eq!(headless.present(&frame(&pixels)), Flow::Continue);
        assert_eq!(headless.present(&frame(&pixels)), Flow::Exit(0));
    }

    #[test]
    fn a_malformed_frame_is_not_counted() {
        let mut headless = Headless::new(Some(1));
        let short = [0u8; 3];
        assert_eq!(headless.present(&frame(&short)), Flow::Continue);
        assert_eq!(headless.frames, 0);
    }

    #[test]
    fn the_verdict_line_is_the_documented_one() {
        assert_eq!(
            Headless::verdict(300, 0xdead_beef),
            "SRB2KART:HEADLESS:PASS frames=300 crc=deadbeef"
        );
    }
}

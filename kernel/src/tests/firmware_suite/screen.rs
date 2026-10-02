//! The boot-log ring, the logical screen arithmetic, framebuffer views and
//! the panic report, all against fake framebuffers in RAM with guard bands,
//! so "never writes outside the framebuffer" is checked byte for byte.

use super::*;
use crate::display::logical::{self, MAX_HEIGHT, MAX_WIDTH};
use crate::gfx::{self, Color, Framebuffer};
use crate::klog::{self, Ring};
use bootloader_api::info::{FrameBufferInfo, PixelFormat};

/// Bytes of guard band around a fake framebuffer.
const GUARD: usize = 4096;
const GUARD_BYTE: u8 = 0xA7;

/// A fake framebuffer in heap memory with guard bands on both sides.
struct FakeFb {
    memory: Vec<u8>,
    info: FrameBufferInfo,
}

impl FakeFb {
    fn new(width: usize, height: usize, stride: usize, bpp: usize, format: PixelFormat) -> Self {
        let byte_len = stride * height * bpp;
        let mut memory = vec![GUARD_BYTE; byte_len + 2 * GUARD];
        memory[GUARD..GUARD + byte_len].fill(0);
        FakeFb {
            memory,
            info: FrameBufferInfo {
                byte_len,
                width,
                height,
                pixel_format: format,
                bytes_per_pixel: bpp,
                stride,
            },
        }
    }

    fn framebuffer(&mut self) -> Framebuffer {
        Framebuffer::new(self.memory.as_mut_ptr() as usize + GUARD, self.info)
    }

    fn guards_intact(&self) -> bool {
        let end = self.memory.len() - GUARD;
        self.memory[..GUARD].iter().all(|&b| b == GUARD_BYTE)
            && self.memory[end..].iter().all(|&b| b == GUARD_BYTE)
    }

    /// Whether every pixel outside `(x, y, w, h)` is still zero (stride
    /// padding included).
    fn untouched_outside(&self, x: usize, y: usize, w: usize, h: usize) -> bool {
        let bpp = self.info.bytes_per_pixel;
        let pixels = &self.memory[GUARD..GUARD + self.info.byte_len];
        pixels
            .chunks(self.info.stride * bpp)
            .enumerate()
            .all(|(row, bytes)| {
                bytes.chunks(bpp).enumerate().all(|(col, pixel)| {
                    let inside = row >= y && row < y + h && col >= x && col < x + w;
                    inside || pixel.iter().all(|&b| b == 0)
                })
            })
    }
}

/// The plan's cases: 4K is cut to 1080p centred, 2560x1600 per axis with
/// uneven borders, 640x480 used as is, odd leftovers, and degenerate modes.
pub fn logical_fit_cases() -> Result<(), String> {
    let cases = [
        ((3840, 2160), (960, 540, 1920, 1080)),
        ((2560, 1600), (320, 260, 1920, 1080)),
        ((2560, 1440), (320, 180, 1920, 1080)),
        ((640, 480), (0, 0, 640, 480)),
        ((1920, 1200), (0, 60, 1920, 1080)),
        ((1921, 1081), (0, 0, 1920, 1080)),
        ((1280, 720), (0, 0, 1280, 720)),
        ((0, 0), (0, 0, 0, 0)),
        ((7680, 4320), (2880, 1620, 1920, 1080)),
    ];
    for ((width, height), (x, y, w, h)) in cases {
        let got = logical::fit(width, height);
        check!(
            (got.x, got.y, got.width, got.height) == (x, y, w, h),
            "{width}x{height} -> {got:?}"
        );
        check!(
            got.x + got.width <= width && got.y + got.height <= height,
            "{width}x{height} overflows"
        );
    }
    check!(
        logical::fit(3840, 2160).rgba_bytes() <= crate::ipc::shared::MAX_BUFFER_BYTES_PER_PROCESS,
        "the cap does not fit the shared-buffer budget"
    );
    check!((MAX_WIDTH, MAX_HEIGHT) == (1920, 1080), "cap changed");
    Ok(())
}

/// Blits into the logical view of a 4K-shaped fake (scaled down 4x to fit
/// the heap: a 960x540 mode with a 480x270 view at (240, 135), and a stride
/// wider than the mode) at every kind of offset never touch a byte outside
/// the view, in RGB and BGR, at 4 and 3 bytes per pixel.
pub fn view_blits_stay_inside() -> Result<(), String> {
    let source = vec![0xC8u8; 600 * 400 * 4];
    for (bpp, format) in [
        (4, PixelFormat::Bgr),
        (4, PixelFormat::Rgb),
        (3, PixelFormat::Rgb),
    ] {
        let mut fake = FakeFb::new(960, 540, 1000, bpp, format);
        let (vx, vy, vw, vh) = (240, 135, 480, 270);
        let mut seed = 0x0123_4567_89AB_CDEFu64;
        for round in 0..600 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let field = |shift: u32, modulo: u64| ((seed >> shift) % modulo) as usize;
            let (dx, dy) = (field(0, 700), field(10, 500));
            let (sx, sy) = (field(20, 50), field(30, 50));
            let (w, h) = (field(40, 600) + 1, field(50, 400) + 1);
            let mut view = fake.framebuffer().view(vx, vy, vw, vh);
            view.blit_rgba_region(&source, 600, 400, sx, sy, dx, dy, w, h);
            if round % 50 == 0 {
                view.fill_rect(dx, dy, w, h, Color::rgb(1, 2, 3));
                view.write_pixel(dx, dy, Color::rgb(9, 9, 9));
            }
        }
        check!(fake.guards_intact(), "bpp {bpp}: guard band written");
        check!(
            fake.untouched_outside(vx, vy, vw, vh),
            "bpp {bpp} {format:?}: wrote outside the view"
        );
        // The view's corner pixel was reached and holds the source colour in
        // the right byte order.
        let mut view = fake.framebuffer().view(vx, vy, vw, vh);
        view.blit_rgba_region(&[10, 20, 30, 255], 1, 1, 0, 0, 0, 0, 1, 1);
        let at = GUARD + (vy * 1000 + vx) * bpp;
        let want: [u8; 3] = if format == PixelFormat::Bgr {
            [30, 20, 10]
        } else {
            [10, 20, 30]
        };
        check!(
            fake.memory[at..at + 3] == want,
            "bpp {bpp} {format:?}: corner {:?}",
            &fake.memory[at..at + 3]
        );
    }
    // A view placed or sized past the framebuffer is clamped to it.
    let mut fake = FakeFb::new(64, 32, 64, 4, PixelFormat::Rgb);
    let view = fake.framebuffer().view(60, 30, 100, 100);
    check!(
        (view.width(), view.height()) == (4, 2),
        "clamped view {}x{}",
        view.width(),
        view.height()
    );
    let mut off = fake.framebuffer().view(500, 500, 10, 10);
    off.fill_rect(0, 0, 10, 10, Color::rgb(255, 255, 255));
    check!(
        fake.guards_intact() && fake.untouched_outside(0, 0, 0, 0),
        "an off-screen view wrote"
    );
    Ok(())
}

/// Hostile firmware geometry is clamped before any write: a stride smaller
/// than the width, a `byte_len` shorter than the rows, 0 or 9 bytes per pixel.
pub fn framebuffer_geometry_sanitized() -> Result<(), String> {
    let base = FrameBufferInfo {
        byte_len: 100 * 50 * 4,
        width: 100,
        height: 50,
        pixel_format: PixelFormat::Rgb,
        bytes_per_pixel: 4,
        stride: 100,
    };
    let narrow = gfx::sanitize(FrameBufferInfo { stride: 80, ..base });
    check!(
        narrow.width == 80 && narrow.height == 50,
        "stride < width: {narrow:?}"
    );
    let short = gfx::sanitize(FrameBufferInfo {
        byte_len: 100 * 4 * 10 + 7,
        ..base
    });
    check!(short.height == 10, "short buffer: {short:?}");
    for bpp in [0usize, 9] {
        let odd = gfx::sanitize(FrameBufferInfo {
            bytes_per_pixel: bpp,
            ..base
        });
        check!(odd.width == 0 && odd.height == 0, "bpp {bpp}: {odd:?}");
    }
    // Drawing through a lying geometry stays inside the real bytes.
    let mut fake = FakeFb::new(100, 10, 100, 4, PixelFormat::Rgb);
    let mut lying = fake.info;
    lying.height = 5000;
    lying.width = 400;
    let mut fb = Framebuffer::new(fake.memory.as_mut_ptr() as usize + GUARD, lying);
    fb.fill_rect(0, 0, 5000, 5000, Color::rgb(1, 1, 1));
    fb.scroll_up(3, Color::rgb(2, 2, 2));
    check!(
        fake.guards_intact(),
        "a lying geometry wrote past the buffer"
    );
    // 8-bit grey (VBE packed pixel) writes one byte per pixel.
    let mut grey = FakeFb::new(16, 4, 16, 1, PixelFormat::U8);
    grey.framebuffer()
        .fill_rect(0, 0, 16, 4, Color::rgb(255, 255, 255));
    check!(grey.guards_intact(), "a 1-byte mode was overrun");
    check!(
        grey.memory[GUARD] > 250,
        "grey value {}",
        grey.memory[GUARD]
    );
    Ok(())
}

/// The ring keeps the newest bytes in order across many wraps; `tail_lines`
/// picks whole last lines; interleaved writers never split a record.
pub fn klog_ring_wraps_and_tails() -> Result<(), String> {
    let mut ring: Ring<256> = Ring::new();
    let mut expected = Vec::new();
    for i in 0..5000u32 {
        let record = format!("w{} line {i}\n", i % 3);
        ring.push(record.as_bytes());
        expected.extend_from_slice(record.as_bytes());
    }
    let mut out = [0u8; 256];
    let count = ring.copy_to(&mut out);
    check!(count == 256, "held {count}");
    check!(
        out[..] == expected[expected.len() - 256..],
        "ring content is not the newest bytes"
    );
    check!(
        ring.total() == expected.len() as u64,
        "total {}",
        ring.total()
    );
    let mut small = [0u8; 10];
    check!(ring.copy_to(&mut small) == 10, "short copy");
    check!(
        small[..] == expected[expected.len() - 10..],
        "short copy is not the newest"
    );
    // An oversized single write keeps its tail.
    let big: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    ring.push(&big);
    ring.copy_to(&mut out);
    check!(out[..] == big[1000 - 256..], "oversized write");
    // Tails.
    check!(klog::tail_lines(b"a\nb\nc\n", 2) == b"b\nc", "tail 2");
    check!(
        klog::tail_lines(b"a\nb\nc", 5) == b"a\nb\nc",
        "tail more than held"
    );
    check!(klog::tail_lines(b"a\nb\n", 0).is_empty(), "tail 0");
    // The live ring ends with the newest serial line and holds as much of
    // the log as fits. (The boot's HW: lines have long scrolled out by the
    // time the suite gets here; the serial log itself shows them.)
    crate::serial_println!("klog: live ring marker 0x5EED");
    let mut live = vec![0u8; klog::CAPACITY];
    let held = klog::snapshot(&mut live);
    let expect = (klog::total() as usize).min(klog::CAPACITY);
    check!(held == expect, "held {held}, expected {expect}");
    check!(
        live[..held].ends_with(b"klog: live ring marker 0x5EED\n"),
        "the live ring does not end with the newest line"
    );
    Ok(())
}

/// Stress: many writers' records through the live ring (each `serial_print`
/// is one record) come back whole and in order, and the ring never grows.
pub fn klog_soak_live_ring() -> Result<(), String> {
    let before = klog::total();
    crate::serial_println!("klog: soak starting");
    check!(
        klog::total() > before,
        "serial_println did not reach the ring"
    );
    for i in 0..4000u32 {
        klog::write_fmt(format_args!(
            "klog_soak:PROGRESS:writer {} record {i:05}\n",
            i % 4
        ));
    }
    check!(klog::total() > before, "the ring did not count the writes");
    let mut live = vec![0u8; klog::CAPACITY];
    let held = klog::snapshot(&mut live);
    check!(held == klog::CAPACITY, "ring holds {held} after a flood");
    let text = core::str::from_utf8(&live[..held]).map_err(|_| "ring is not UTF-8")?;
    let mut last = None;
    for line in text
        .lines()
        .filter(|l| l.contains("klog_soak:PROGRESS:writer"))
        .skip(1)
    {
        let number: u32 = line
            .rsplit(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("torn record: {line}"))?;
        if let Some(previous) = last {
            check!(number == previous + 1, "record {number} after {previous}");
        }
        last = Some(number);
    }
    check!(last == Some(3999), "last record {last:?}");
    Ok(())
}

/// The panic report renders into a fake framebuffer: the screen turns the
/// report colour, text pixels appear, the guard bands hold, and it works at
/// every scale step and with a huge message.
pub fn panic_report_renders() -> Result<(), String> {
    check!(crate::panic_screen::scale_for(1280) == 1, "scale at 1280");
    check!(crate::panic_screen::scale_for(2560) == 2, "scale at 2560");
    check!(crate::panic_screen::scale_for(3840) == 3, "scale at 3840");
    let pieces: Vec<&str> = crate::panic_screen::wrap("abcdefgh", 3).collect();
    check!(pieces == ["abc", "def", "gh"], "wrap {pieces:?}");
    check!(crate::panic_screen::wrap("", 3).count() == 1, "empty wrap");
    let mut log = Vec::new();
    for i in 0..400 {
        log.extend_from_slice(format!("boot line {i} with some words\n").as_bytes());
    }
    let long = "x".repeat(5000);
    for (width, height, bpp) in [(800, 600, 4), (640, 480, 3)] {
        let mut fake = FakeFb::new(width, height, width + 16, bpp, PixelFormat::Bgr);
        let mut fb = fake.framebuffer();
        crate::panic_screen::render(&mut fb, "LazyOS stopped: test", &long, &log);
        check!(fake.guards_intact(), "{width}x{height}: guard band written");
        let corner = fb.read_pixel(width - 1, height - 1);
        check!(
            (corner.r, corner.g, corner.b) == (0x50, 0x0A, 0x0A),
            "corner {corner:?}"
        );
        let bright = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| fb.read_pixel(x, y).g > 0x80)
            .count();
        check!(bright > 500, "{width}x{height}: only {bright} text pixels");
    }
    Ok(())
}

/// The boot remapped the framebuffer write-combining (PAT entry 7 is WC and
/// every leaf selects it) and pixels still read back what was written; a
/// second remap is idempotent.
pub fn framebuffer_is_write_combining() -> Result<(), String> {
    let (base, len) = crate::console::framebuffer_span().ok_or("no framebuffer")?;
    let pages = len.div_ceil(4096);
    for page in [0, pages / 2, pages - 1] {
        check!(
            crate::mem::wc::is_write_combining(base + page * 4096),
            "page {page} of {pages} is not write-combining"
        );
    }
    let again = crate::mem::wc::map_write_combining(base, len).map_err(to_string)?;
    check!(again == pages, "remapped {again} of {pages}");
    let color = Color::rgb(0x21, 0x43, 0x65);
    let back = crate::console::with_framebuffer(|fb| {
        let old = fb.read_pixel(3, 3);
        fb.write_pixel(3, 3, color);
        let got = fb.read_pixel(3, 3);
        fb.write_pixel(3, 3, old);
        got
    })
    .ok_or("no framebuffer")?;
    check!(
        (back.r, back.g, back.b) == (0x21, 0x43, 0x65),
        "read back {back:?}"
    );
    Ok(())
}

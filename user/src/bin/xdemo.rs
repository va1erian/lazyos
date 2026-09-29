//! `xdemo`: the display protocol demo app (issue #113).
//!
//! The smallest real client of `xuid`'s `os.lazy.display.v1`: it creates one
//! surface, asks the display syscall for a shared pixel buffer, paints a
//! recognizable colour grid and a status line, attaches the buffer and commits.
//! Input forwarded by the compositor updates the picture: a click advances the
//! palette, a key press bumps the key counter and the last key code, and the
//! pointer leaves a marker square.
//!
//! Boot it with `LAZYOS_XUID=1`; the kernel starts `xuid` first.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use user::messenger;
use user::messenger::display::{self, Canvas, Client, Color, Event, Rect};
use user::sys;

/// Surface content size in pixels.
const W: i32 = 360;
const H: i32 = 220;
/// Grid cell size in pixels.
const CELL: i32 = 24;

/// Current demo state; redrawn from scratch after every input event.
struct Demo {
    /// Clicks delivered by the compositor.
    clicks: u64,
    /// Key presses delivered by the compositor.
    keys: u64,
    /// Last key code, or -1 before any key.
    last_key: i64,
    /// Last pointer position inside the surface, or `None`.
    marker: Option<(i64, i64)>,
}

impl Demo {
    fn new() -> Demo {
        Demo {
            clicks: 0,
            keys: 0,
            last_key: -1,
            marker: None,
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("xdemo: display client starting (issue #113)\n");
    run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("xdemo: panic\n");
    sys::exit(1)
}

/// Connect, create the surface, and poll input forever.
fn run() -> ! {
    // The compositor may still be binding/registering, so the client retries.
    let client = match Client::connect() {
        Ok(client) => client,
        Err(error) => fail("connect", error.errno().unwrap_or(0)),
    };

    // The app keeps one end of a private pair and hands the other to the
    // compositor in `CreateSurface`; input arrives as one-way messages here.
    let (events, events_server) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", error.errno().unwrap_or(0)),
    };

    let surface = match client.create_surface(W as u64, H as u64, "xdemo", &events_server) {
        Ok(surface) => surface,
        Err(error) => fail("create_surface", error.errno().unwrap_or(0)),
    };

    let bytes = (W * H * 4) as u64;
    let (buffer, va) = match sys::display_create_buffer(bytes) {
        Ok(buffer) => buffer,
        Err(code) => fail("create_buffer", code),
    };
    // Safety: `va` is the mapping of the buffer just created, `W * H * 4`
    // bytes long.
    let mut canvas = unsafe { Canvas::new(va, W, H) };
    let full = Rect::new(0, 0, W, H);
    let mut demo = Demo::new();
    draw(&mut canvas, &demo, full);

    if let Err(error) = client.attach_buffer(surface, buffer, bytes) {
        fail("attach_buffer", error.errno().unwrap_or(0));
    }
    if let Err(error) = client.commit(surface, full) {
        fail("commit", error.errno().unwrap_or(0));
    }
    sys::write_str("XDEMO:UP:PASS\n");

    // One receive buffer for the whole life of the loop: the user bump
    // allocator never reclaims, and a service loop must not allocate per
    // message.
    let mut buf = alloc::vec![0u8; 4096];
    loop {
        let deadline = Some(sys::clock() + 1);
        match events.recv_with(&mut buf, deadline) {
            Ok(message) => {
                // The compositor's title-bar close button asks the app to go
                // away (issue #143); there is nothing to draw into any more.
                if message.method() == display::wire::METHOD_WINDOWCLOSE {
                    sys::write_str("xdemo: closed by the window manager\n");
                    sys::exit(0);
                }
                if let Some(event) = display::decode_event(&message) {
                    apply(&mut demo, event);
                    draw(&mut canvas, &demo, full);
                    let _ = client.commit(surface, full);
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE => {
                // The compositor died; there is nothing left to draw into.
                sys::exit(0)
            }
            Err(_) => {}
        }
    }
}

/// Report a fatal startup failure on serial, then exit.
fn fail(what: &str, code: i64) -> ! {
    sys::write_str("xdemo: fatal: ");
    sys::write_str(what);
    sys::write_str(": ");
    sys::write_str(&alloc::format!("{code}\n"));
    sys::exit(1)
}

/// Whether an error is the `recv` deadline firing.
fn is_timeout(error: messenger::Error) -> bool {
    matches!(error, messenger::Error::Errno(code) if code == -messenger::errno::ETIMEDOUT)
}

/// Fold one compositor event into the demo state.
fn apply(demo: &mut Demo, event: Event) {
    match event {
        Event::PointerMove { x, y } => demo.marker = Some((x as i64, y as i64)),
        Event::PointerDown { .. } => demo.clicks += 1,
        Event::PointerUp { .. } | Event::KeyUp { .. } => {}
        Event::KeyDown { key } => {
            demo.keys += 1;
            demo.last_key = key as i64;
        }
    }
}

/// Paint the whole demo into the app's buffer, clipped to `clip`.
fn draw(canvas: &mut Canvas, demo: &Demo, clip: Rect) {
    let full = Rect::new(0, 0, W, H);
    canvas.fill(full, clip, Color::rgb(12, 14, 24));

    // A colour grid whose phase advances on every click, so an input event is
    // unmistakable in a screenshot.
    let palette = [
        Color::rgb(200, 70, 70),
        Color::rgb(70, 170, 90),
        Color::rgb(70, 110, 210),
    ];
    let phase = (demo.clicks % 3) as usize;
    let mut row = 0;
    let mut y = 0;
    while y < H {
        let mut col = 0;
        let mut x = 0;
        while x < W {
            let tone = if (row + col) % 2 == 0 { 1.0 } else { 0.55 };
            let color = scale(palette[(row + col + phase) % palette.len()], tone);
            let cell = Rect::new(x, y, (W - x).min(CELL), (H - y).min(CELL));
            canvas.fill(cell, clip, color);
            col += 1;
            x += CELL;
        }
        row += 1;
        y += CELL;
    }

    // Status band so the text path is visible too.
    let band = Rect::new(0, 0, W, 30);
    canvas.fill(band, clip, Color::rgb(8, 10, 18));
    canvas.text(8, 6, "LAZYOS XUID DEMO", Color::rgb(240, 244, 255), clip, 1);
    canvas.text(
        208,
        6,
        &alloc::format!("C {} K {}", demo.clicks, demo.keys),
        Color::rgb(160, 230, 180),
        clip,
        1,
    );
    canvas.text(
        8,
        36,
        &alloc::format!(
            "KEY {} MOVE {}",
            demo.last_key,
            demo.marker.map(|m| m.0).unwrap_or(-1)
        ),
        Color::rgb(220, 224, 235),
        clip,
        1,
    );
    canvas.text(
        8,
        H - 16,
        "CLICK ME / PRESS A KEY",
        Color::rgb(230, 200, 120),
        clip,
        1,
    );

    // The pointer marker the compositor reports, if any.
    if let Some((mx, my)) = demo.marker {
        let rect = Rect::new(mx as i32 - 4, my as i32 - 4, 8, 8);
        canvas.fill(rect, clip, Color::rgb(255, 255, 255));
        canvas.fill(
            Rect::new(rect.x + 2, rect.y + 2, 4, 4),
            clip,
            Color::rgb(20, 20, 20),
        );
    }
}

/// Multiply a colour's channels by `factor`, clamped to 255.
fn scale(color: Color, factor: f32) -> Color {
    let apply = |value: u8| -> u8 {
        let scaled = value as f32 * factor;
        if scaled > 255.0 {
            255
        } else if scaled < 0.0 {
            0
        } else {
            scaled as u8
        }
    };
    Color::rgb(apply(color.r), apply(color.g), apply(color.b))
}

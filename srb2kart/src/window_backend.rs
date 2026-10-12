//! The desktop [`Backend`]: the engine's frames and keys on an `xuid` window
//! (`window.rs`), the clock from a start `Instant`.

use std::time::Instant;

use lazysrb2kart::backend::{Backend, Event, Flow, Frame};

use crate::window::Window;

/// Input events reported on the serial line (`SRB2KART:EVENT:<kind>:<key>`).
const LOGGED_EVENTS: u32 = 64;

/// The window [`WindowBackend::bootstrap`] opens before the engine starts: the
/// default video mode (`i_video.c`), so the engine's switch to it is no resize.
const BOOT_W: usize = 640;
const BOOT_H: usize = 400;
/// The engine's first mode, which it leaves as soon as its config is read;
/// while loading, the bootstrap window is not shrunk to it.
const BASE_MODE: (usize, usize) = (320, 200);

pub struct WindowBackend {
    window: Option<Window>,
    start: Instant,
    /// Frames presented in the current second, for the `FPS` marker.
    frames: u32,
    second: u64,
    logged_events: u32,
    /// Between [`WindowBackend::bootstrap`] and the engine's `ready`: the
    /// pointer is busy over the window.
    loading: bool,
}

impl WindowBackend {
    pub fn new() -> WindowBackend {
        WindowBackend {
            window: None,
            start: Instant::now(),
            frames: 0,
            second: 0,
            logged_events: 0,
            loading: false,
        }
    }

    /// Open the window before the engine starts, black, with the busy pointer:
    /// loading the data takes seconds (issue #768) and the engine opens its
    /// own window only after it. A failure is left to the engine's own open.
    pub fn bootstrap(&mut self) {
        let Ok(mut window) = Window::open(BOOT_W, BOOT_H, "SRB2Kart") else {
            return;
        };
        let black = [0u8; 768];
        window.present(&vec![0; BOOT_W * BOOT_H], BOOT_W, BOOT_H, BOOT_W, &black);
        window.set_busy(true);
        self.window = Some(window);
        self.loading = true;
        println!("SRB2KART:LOADING:BUSY");
    }
}

impl Backend for WindowBackend {
    fn open(&mut self, width: usize, height: usize, title: &str) -> Result<(), String> {
        match self.window.as_mut() {
            // The engine re-opens on a video mode change: keep the window.
            Some(window) => {
                window.set_title(title);
                if !(self.loading && (width, height) == BASE_MODE) {
                    window.request_size(width, height);
                }
            }
            None => {
                let window =
                    Window::open(width, height, title).map_err(|error| format!("{error:?}"))?;
                self.window = Some(window);
            }
        }
        Ok(())
    }

    fn present(&mut self, frame: &Frame<'_>) -> Flow {
        if let Some(window) = self.window.as_mut() {
            window.present(
                frame.pixels,
                frame.width,
                frame.height,
                frame.pitch,
                frame.palette,
            );
        }
        self.frames += 1;
        let second = self.start.elapsed().as_secs();
        if second != self.second {
            println!(
                "SRB2KART:FPS:{}:{}x{}",
                self.frames, frame.width, frame.height
            );
            self.frames = 0;
            self.second = second;
        }
        Flow::Continue
    }

    fn poll(&mut self) -> Option<Event> {
        let event = self.window.as_mut()?.poll()?;
        // Evidence that input arrives; bounded so a long game is not noisy.
        if self.logged_events < LOGGED_EVENTS {
            self.logged_events += 1;
            println!("SRB2KART:EVENT:{}:{}", event.kind, event.key);
        }
        Some(event)
    }

    fn ticks_ns(&mut self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn sleep(&mut self, us: u32) {
        xui_app::sys::sleep_millis(u64::from(us.div_ceil(1000)));
    }

    fn ready(&mut self) {
        if std::mem::take(&mut self.loading) {
            if let Some(window) = self.window.as_mut() {
                window.set_busy(false);
            }
            println!(
                "SRB2KART:LOADING:DONE ms={}",
                self.start.elapsed().as_millis()
            );
        }
    }

    fn close(&mut self) {
        if let Some(mut window) = self.window.take() {
            window.close();
        }
    }
}

//! The desktop [`Backend`]: the engine's frames and keys on an `xuid` window
//! (`window.rs`), the clock from a start `Instant`.

use std::time::Instant;

use lazysrb2kart::backend::{Backend, Event, Flow, Frame};

use crate::window::Window;

pub struct WindowBackend {
    window: Option<Window>,
    start: Instant,
    /// Frames presented in the current second, for the `FPS` marker.
    frames: u32,
    second: u64,
}

impl WindowBackend {
    pub fn new() -> WindowBackend {
        WindowBackend {
            window: None,
            start: Instant::now(),
            frames: 0,
            second: 0,
        }
    }
}

impl Backend for WindowBackend {
    fn open(&mut self, width: usize, height: usize, title: &str) -> Result<(), String> {
        match self.window.as_mut() {
            // The engine re-opens on a video mode change: keep the window.
            Some(window) => {
                window.set_title(title);
                window.request_size(width, height);
            }
            None => {
                let window = Window::open(width, height, title)
                    .map_err(|error| format!("{error:?}"))?;
                self.window = Some(window);
            }
        }
        Ok(())
    }

    fn present(&mut self, frame: &Frame<'_>) -> Flow {
        if let Some(window) = self.window.as_mut() {
            window.present(frame.pixels, frame.width, frame.height, frame.pitch, frame.palette);
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
        self.window.as_mut()?.poll()
    }

    fn ticks_ns(&mut self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn sleep(&mut self, us: u32) {
        xui_app::sys::sleep_millis(u64::from(us.div_ceil(1000)));
    }

    fn close(&mut self) {
        if let Some(mut window) = self.window.take() {
            window.close();
        }
    }
}

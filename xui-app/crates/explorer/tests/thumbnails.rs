//! A real folder of pictures, headless: thumbnails are made on worker threads
//! and drawn in place of the generic icon once they arrive. The render is
//! written to `target/snapshots/xui-explorer-thumbnails-{light,dark}.png`.
#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use std::cell::RefCell;

use xui_canvas::OffscreenBackend;
use xui_core::app::{Ui, run_app};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_core::{Image, Theme};
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::Msg;
use xui_explorer::{Explorer, ExplorerWindow, StdPlatform};

struct NoLauncher;

impl Launcher for NoLauncher {
    fn open(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// A `width` x `height` PNG: a magenta-to-cyan diagonal ramp, so a drawn
/// thumbnail is unmistakable against the grey generic icon and the aspect
/// ratio is visible.
fn ramp(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let t = ((x + y) * 255 / (width + height)) as u8;
            pixels.extend_from_slice(&[255 - t, t, 255, 255]);
        }
    }
    Image::from_rgba(width, height, pixels)
        .unwrap()
        .encode_png()
        .unwrap()
}

fn picture_folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xui-explorer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, w, h) in [
        ("landscape.png", 1600, 900),
        ("portrait.png", 600, 1000),
        ("square.png", 400, 400),
        ("panorama.png", 2400, 300),
        ("tiny.png", 24, 24),
    ] {
        std::fs::write(dir.join(file), ramp(w, h)).unwrap();
    }
    std::fs::write(dir.join("notes.txt"), "not a picture").unwrap();
    std::fs::write(dir.join("broken.png"), b"\x89PNG\r\n\x1a\nnope").unwrap();
    dir
}

/// Paints the window twice on the offscreen backend, as a real one would
/// repaint: the first frame shows the generic icons and queues the
/// thumbnails; after the workers had their moment the window gets the wake
/// they post, and the second frame is returned.
fn render(theme: Theme, dir: PathBuf) -> (Image, Image) {
    let platform: Rc<dyn Platform> = Rc::new(StdPlatform::new());
    let backend = Rc::new(OffscreenBackend::with_dpi(96));
    let ui_slot: Rc<RefCell<Option<Ui<Msg>>>> = Rc::default();
    let frames: Rc<RefCell<Vec<Image>>> = Rc::default();
    {
        let weak = Rc::downgrade(&backend);
        let ui_slot = Rc::clone(&ui_slot);
        let frames = Rc::clone(&frames);
        backend.set_run_hook(move || {
            let (Some(backend), Some(ui)) = (weak.upgrade(), ui_slot.borrow_mut().take()) else {
                return;
            };
            let window = ui.window();
            let grab = |backend: &OffscreenBackend| {
                let frame = backend.render(window).expect("a frame");
                Image::from_rgba(frame.width, frame.height, frame.pixels).unwrap()
            };
            frames.borrow_mut().push(grab(&backend));
            // Deliver the workers' wake until the frame shows them, so a slow
            // runner takes longer rather than failing.
            let mut after = grab(&backend);
            for _ in 0..200 {
                if ramp_pixels(&after) > 300 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
                ui.emit(Msg::Thumbs);
                backend.pump(window);
                after = grab(&backend);
            }
            frames.borrow_mut().push(after);
        });
    }
    let handle: Rc<dyn Backend> = backend.clone();
    let spec = PlatformSpec::new("thumbnails").size(Dip(560.0), Dip(360.0));
    let slot = Rc::clone(&ui_slot);
    run_app(handle, spec, move |ui| {
        ui.set_theme(theme);
        let explorer = Explorer::new(platform, Rc::new(NoLauncher));
        let window = ExplorerWindow::new(ui, explorer, dir).expect("a window");
        *slot.borrow_mut() = Some(ui.clone());
        window
    })
    .expect("the app runs");
    let mut frames = frames.take();
    assert_eq!(frames.len(), 2);
    let after = frames.pop().unwrap();
    (frames.pop().unwrap(), after)
}

/// Pixels close to the ramp's magenta end.
fn ramp_pixels(image: &Image) -> usize {
    image
        .pixels()
        .chunks(4)
        .filter(|p| p[0] > 200 && p[1] < 80 && p[2] > 200)
        .count()
}

#[test]
fn pictures_show_their_thumbnails_on_both_themes() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&out).unwrap();
    let dir = picture_folder("thumbs");
    for (label, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        let (before, image) = render(theme, dir.clone());
        assert_eq!(
            ramp_pixels(&before),
            0,
            "{label}: the first frame is generic"
        );
        assert!(
            ramp_pixels(&image) > 300,
            "{label}: thumbnails were drawn ({} ramp pixels)",
            ramp_pixels(&image)
        );
        image
            .save_png(out.join(format!("xui-explorer-thumbnails-{label}.png")))
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

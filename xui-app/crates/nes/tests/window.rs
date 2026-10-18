//! The NES Player window driven offscreen: the welcome screen, loading the
//! sample ROM, the console running into a picture, a save state, and a
//! snapshot for a human to look at (`target/snapshots/nes-*.png`).
//!
//! The control's own timer does not run offscreen, so the test drives
//! `Screen::state().tick()` the way the timer would; everything else is the
//! real app: the menus, the reports and the canvas.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::image::Image;
use xui_core::{Dip, Theme};
use xui_nes::host::{Host, Report};
use xui_nes::screen::Screen;
use xui_nes::{Command, Msg, NesApp, Session, WINDOW};

const SAMPLE: &[u8] = include_bytes!("data/sample.nes");

/// A unique folder under the temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("nes-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fake platform: audio is counted, reports are collected.
struct Fake {
    dir: PathBuf,
    reports: Rc<RefCell<Vec<Report>>>,
    audio_frames: Rc<RefCell<usize>>,
    muted: bool,
}

impl Host for Fake {
    fn write_audio(&mut self, samples: &[i16]) {
        *self.audio_frames.borrow_mut() += samples.len();
    }
    fn audio_free(&mut self) -> Option<usize> {
        Some(8192)
    }
    fn audio_rate(&self) -> u32 {
        48_000
    }
    fn audio_channels(&self) -> u32 {
        1
    }
    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }
    fn muted(&self) -> bool {
        self.muted
    }
    fn report(&self, report: Report) {
        self.reports.borrow_mut().push(report);
    }
    fn app_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    fn default_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    fn sample_rom(&self) -> Option<PathBuf> {
        None
    }
}

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Run `test` on its own thread with the fonts, failing if it hangs.
fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(_) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test hung"),
        },
    }
}

/// What the step closure gets: the window's reports, the mixer's sample
/// count, and the screen (filled once the app is built).
struct Rig {
    reports: Rc<RefCell<Vec<Report>>>,
    audio: Rc<RefCell<usize>>,
    screen: Rc<RefCell<Option<Rc<Screen>>>>,
}

impl Rig {
    fn screen(&self) -> Rc<Screen> {
        self.screen.borrow().clone().expect("the app is built")
    }

    /// Clocks the console the way the control's timer does, until `frames`
    /// display frames have run.
    fn run(&self, stage: &Stage<'_, Msg>, frames: u64) {
        let state = self.screen().state();
        let started = Instant::now();
        while state.borrow().session().frames() < frames {
            state.borrow_mut().tick();
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the console never ran {frames} frames"
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        let _ = stage;
    }

    fn saw(&self, want: impl Fn(&Report) -> bool) -> bool {
        self.reports.borrow().iter().any(want)
    }
}

/// Render the window after `step` drives it.
fn drive(dir: PathBuf, step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static) -> Image {
    let rig = Rig {
        reports: Rc::default(),
        audio: Rc::default(),
        screen: Rc::default(),
    };
    let build_rig = Rig {
        reports: Rc::clone(&rig.reports),
        audio: Rc::clone(&rig.audio),
        screen: Rc::clone(&rig.screen),
    };
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(Theme::dark()),
        move |ui| {
            let host = Fake {
                dir: dir.clone(),
                reports: Rc::clone(&build_rig.reports),
                audio_frames: Rc::clone(&build_rig.audio),
                muted: false,
            };
            let session = Session::new(dir.clone(), 48_000.0);
            let app = NesApp::build(ui, Rc::new(RefCell::new(host)), session, None)?;
            *build_rig.screen.borrow_mut() = Some(app.screen());
            Ok(app)
        },
        move |stage| step(stage, &rig),
    )
    .expect("the headless render")
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// How many distinct colours the window shows (a blank canvas has one).
fn colors(image: &Image) -> usize {
    let mut seen = std::collections::HashSet::new();
    for pixel in image.pixels().chunks_exact(4) {
        seen.insert((pixel[0], pixel[1], pixel[2]));
    }
    seen.len()
}

#[test]
fn the_welcome_screen_offers_a_rom_and_the_picture_plays_it() {
    watchdog(|| {
        let dir = TempDir::new("play");
        let rom = dir.0.join("sample.nes");
        std::fs::write(&rom, SAMPLE).unwrap();
        // The welcome screen, captured after the step.
        let welcome = drive(dir.0.clone(), |_stage, _rig| {});
        assert!(colors(&welcome) > 3, "the welcome screen drew nothing");
        save(&welcome, "nes-welcome.png");

        // The console playing the sample, captured after the step: the image
        // is only painted by a render, so the assertions run on the picture
        // the framework returns.
        let playing = drive(dir.0.clone(), move |stage, rig| {
            stage.emit(Msg::RomChosen(rom.clone()));
            assert!(
                rig.saw(|report| matches!(
                    report,
                    Report::Loaded {
                        mapper: 0,
                        prg_kb: 32,
                        chr_kb: 8,
                        battery: true,
                        region: "ntsc",
                        ..
                    }
                )),
                "the sample ROM loaded: {:?}",
                rig.reports.borrow()
            );

            rig.run(stage, 130);
            // The app drains the screen's reports on any message (the 1 Hz
            // tick is what the timer raises on LazyOS).
            stage.emit(Msg::Tick);
            assert!(
                rig.saw(|report| matches!(report, Report::Running { frames } if *frames >= 120)),
                "the console reported running: {:?}",
                rig.reports.borrow()
            );
            assert!(*rig.audio.borrow() > 10_000, "the mixer got samples");

            stage.emit(Msg::Command(Command::SaveState));
            assert!(
                rig.saw(|report| matches!(report, Report::StateSaved { slot: 1, .. })),
                "the state saved: {:?}",
                rig.reports.borrow()
            );
            stage.emit(Msg::Command(Command::LoadState));
            assert!(
                rig.saw(|report| matches!(report, Report::StateLoaded { slot: 1, .. })),
                "the state loaded"
            );
            // A restored console renders its picture again on the next
            // frames; the frame buffer is derived state, not part of the
            // save.
            rig.run(stage, 140);

            // Pause through the menu, as the user would; the window stays up
            // for the capture.
            stage.emit(Msg::Command(Command::Pause));
            assert!(
                rig.screen().state().borrow().session().paused(),
                "the pause command reached the console"
            );
        });
        save(&playing, "nes-play.png");
        // The console's own colours: the backdrop and the title text, which
        // the welcome screen does not have.
        assert!(
            playing
                .pixels()
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [0x00, 0x23, 0x7B]),
            "the console's picture is in the window"
        );
        assert!(
            playing
                .pixels()
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [0xFF, 0xFF, 0xFF]),
            "the sample ROM's text is drawn"
        );
        assert!(colors(&playing) > colors(&welcome) / 2);
    });
}

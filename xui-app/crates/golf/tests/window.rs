//! The LazyGolf window built offscreen: it generates its course a stage per
//! tick, shows the first tee, flies forward on a held `W`, and renders at
//! 96 and 192 DPI (left in `target/snapshots/golf-window-*.png`).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers};
use xui_core::{Dip, Image};
use xui_golf::game::Game;
use xui_golf::{GolfApp, Msg, WINDOW};

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
    match rx.recv_timeout(Duration::from_secs(300)) {
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

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// What the drive closure saw.
#[derive(Default, Debug, Clone)]
struct Seen {
    ready: bool,
    moved_by: f32,
    reports: Vec<String>,
}

fn run(dpi: u32) -> (Image, Seen) {
    let game: Rc<RefCell<Option<Rc<RefCell<Game>>>>> = Rc::default();
    let seen = Rc::new(RefCell::new(Seen::default()));
    let (keep, log) = (Rc::clone(&game), Rc::clone(&seen));
    let drive_game = Rc::clone(&game);
    let drive_seen = Rc::clone(&seen);
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).dpi(dpi),
        move |ui| {
            let app = GolfApp::build(ui, 11, move |msg| {
                if let Msg::Report(r) = msg {
                    log.borrow_mut().reports.push(format!("{r:?}"));
                }
            })?;
            *keep.borrow_mut() = Some(app.view().game());
            Ok(app)
        },
        move |stage| {
            let game = drive_game.borrow().clone().expect("built");
            let (w, h) = (
                (WINDOW.0 as u32 * stage.dpi() / 96) as usize,
                (WINDOW.1 as u32 * stage.dpi() / 96) as usize,
            );
            let mut now = Instant::now();
            for _ in 0..20 {
                game.borrow_mut().tick(w, h, now);
                if game.borrow().run.is_some() {
                    break;
                }
            }
            let start = game
                .borrow()
                .run
                .as_ref()
                .expect("generated")
                .flyer
                .camera
                .eye;
            drive_seen.borrow_mut().ready = true;
            stage.inject(Event::KeyDown {
                key: Key::W,
                modifiers: Modifiers::default(),
                repeat: 1,
                system: false,
            });
            for _ in 0..10 {
                now += Duration::from_millis(50);
                game.borrow_mut().tick(w, h, now);
            }
            stage.inject(Event::KeyUp {
                key: Key::W,
                modifiers: Modifiers::default(),
                system: false,
            });
            let end = game
                .borrow()
                .run
                .as_ref()
                .expect("generated")
                .flyer
                .camera
                .eye;
            drive_seen.borrow_mut().moved_by = (end - start).length();
        },
    )
    .expect("the headless render");
    let seen = seen.borrow().clone();
    (image, seen)
}

#[test]
fn the_window_generates_flies_and_renders() {
    let (image, seen) = watchdog(|| {
        let (image, seen) = run(96);
        save(&image, "golf-window-96.png");
        (image, seen)
    });
    assert!(seen.ready);
    assert!(seen.moved_by > 5.0, "W did not fly: {seen:?}");
    assert_eq!(image.size(), (WINDOW.0 as u32, WINDOW.1 as u32));
    let mut colours = std::collections::HashSet::new();
    for p in image.pixels().as_chunks::<4>().0.iter().step_by(7) {
        colours.insert([p[0], p[1], p[2]]);
    }
    assert!(colours.len() > 60, "{} colours", colours.len());
}

#[test]
fn the_window_renders_at_twice_the_dpi() {
    let (image, _) = watchdog(|| {
        let (image, seen) = run(192);
        save(&image, "golf-window-192.png");
        (image, seen)
    });
    assert_eq!(image.size(), (WINDOW.0 as u32 * 2, WINDOW.1 as u32 * 2));
}

/// The picture must not jump when the automatic resolution changes: the
/// frame on screen is the one drawn at the old scale until a frame at the
/// new scale replaces it (it was once upscaled with the new scale for a
/// frame, which looked like the camera lurching).
#[test]
fn a_scale_change_never_shows_a_mis_scaled_frame() {
    let mut game = Game::with_params(xui_golf::Params::quick(4), 96);
    let (w, h) = (640, 400);
    let mut now = Instant::now();
    while game.run.is_none() {
        game.tick(w, h, now);
    }
    now += Duration::from_millis(20);
    game.tick(w, h, now);
    let before = game.run.as_ref().unwrap().image.clone().expect("a picture");
    // What the once-a-second adaptation does between two frames.
    game.run.as_mut().unwrap().scale = 2;
    for _ in 0..3 {
        now += Duration::from_millis(150);
        game.tick(w, h, now);
        let after = game.run.as_ref().unwrap().image.clone().expect("a picture");
        let same = before
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(after.pixels().as_chunks::<4>().0)
            .filter(|(a, b)| a == b)
            .count();
        let share = same as f32 / (w * h) as f32;
        // The same view: identical but for blockier pixels at 2x.
        assert!(
            share > 0.5,
            "the picture jumped: only {share:.2} of it matches"
        );
    }
}

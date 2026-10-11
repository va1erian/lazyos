//! Playing through the window's own key events: the swing meter, the three
//! clicks and the shot they report, and the keys that tilt the camera.

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
        // Still running: fail now; joining would wait for the hung thread.
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// A swing through the window's own key events: Space starts the meter, a
/// second Space sets the power at the tick, a third the accuracy. The
/// picture is taken with the meter moving, so the HUD is on it.
fn swing_picture(dpi: u32, click_accuracy: bool) -> (Image, Vec<String>, f32, bool) {
    use xui_golf::play::Phase;
    let game: Rc<RefCell<Option<Rc<RefCell<Game>>>>> = Rc::default();
    let seen = Rc::new(RefCell::new((Vec::<String>::new(), 0.0f32, false)));
    let (keep, log) = (Rc::clone(&game), Rc::clone(&seen));
    let drive_game = Rc::clone(&game);
    let drive_seen = Rc::clone(&seen);
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).dpi(dpi),
        move |ui| {
            let app = GolfApp::build(ui, 11, move |msg| {
                if let Msg::Report(r) = msg {
                    log.borrow_mut().0.push(format!("{r:?}"));
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
            let deadline = Instant::now() + Duration::from_secs(120);
            while game.borrow().run.is_none() && Instant::now() < deadline {
                game.borrow_mut().tick(w, h, now);
                std::thread::sleep(Duration::from_millis(5));
            }
            let space = |down| {
                if down {
                    Event::KeyDown {
                        key: Key::SPACE,
                        modifiers: Modifiers::default(),
                        repeat: 1,
                        system: false,
                    }
                } else {
                    Event::KeyUp {
                        key: Key::SPACE,
                        modifiers: Modifiers::default(),
                        system: false,
                    }
                }
            };
            let step = |game: &Rc<RefCell<Game>>, now: &mut Instant, secs: f32| {
                let n = (secs / 0.02) as usize;
                for _ in 0..n {
                    *now += Duration::from_millis(20);
                    game.borrow_mut().tick(w, h, *now);
                }
            };
            step(&game, &mut now, 0.1);
            let tick = game.borrow().run.as_ref().unwrap().play.target_power(
                &game.borrow().run.as_ref().unwrap().scene.course,
                &game.borrow().run.as_ref().unwrap().scene.bake,
            );
            // Click 1, then wait for the cursor to reach the tick.
            stage.inject(space(true));
            stage.inject(space(false));
            step(&game, &mut now, tick * xui_golf::play::meter::RISE_TIME);
            // Click 2: the power. The cursor now falls toward the line.
            stage.inject(space(true));
            stage.inject(space(false));
            let power = game.borrow().run.as_ref().unwrap().play.phase.clone();
            let locked = match power {
                Phase::Swing(m) => m.power().unwrap_or(0.0),
                _ => -1.0,
            };
            drive_seen.borrow_mut().1 = locked;
            // Fall most of the way: the meter is on screen, mid-accuracy.
            step(
                &game,
                &mut now,
                locked * xui_golf::play::meter::RISE_TIME * 0.5,
            );
            if click_accuracy {
                step(
                    &game,
                    &mut now,
                    locked * xui_golf::play::meter::RISE_TIME * 0.5,
                );
                stage.inject(space(true));
                stage.inject(space(false));
                step(&game, &mut now, 0.5);
                let flying = matches!(
                    game.borrow().run.as_ref().unwrap().play.phase,
                    Phase::Flight { .. }
                );
                drive_seen.borrow_mut().2 = flying;
                step(&game, &mut now, 8.0);
            }
            let reports = {
                let mut g = game.borrow_mut();
                let mut v = Vec::new();
                while let Some(r) = g.take_report() {
                    v.push(format!("{r:?}"));
                }
                v
            };
            drive_seen.borrow_mut().0.extend(reports);
        },
    )
    .expect("the headless render");
    let (reports, locked, flying) = seen.borrow().clone();
    (image, reports, locked, flying)
}

#[test]
fn the_meter_is_drawn_while_the_player_swings() {
    let (image, _, locked, _) = watchdog(|| {
        let r = swing_picture(96, false);
        save(&r.0, "golf-window-meter.png");
        r
    });
    assert!(locked > 0.2 && locked <= 1.0, "power locked at {locked}");
    // The meter's panel is grey at the bottom centre of the window.
    let (w, h) = image.size();
    let px = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [
            image.pixels()[i],
            image.pixels()[i + 1],
            image.pixels()[i + 2],
        ]
    };
    let mut grey = 0;
    for x in (w / 2 - 60)..(w / 2 + 60) {
        for y in (h - 60)..(h - 12) {
            if px(x, y) == [0xC0, 0xC0, 0xC0] {
                grey += 1;
            }
        }
    }
    assert!(
        grey > 300,
        "no meter panel at the bottom: {grey} grey pixels"
    );
}

#[test]
fn three_clicks_send_the_ball_and_report_the_shot() {
    let (_, reports, _, flying) = watchdog(|| swing_picture(96, true));
    assert!(flying, "the ball was in the air after the third click");
    assert!(
        reports.iter().any(|r| r.starts_with("Shot {")),
        "no Shot report: {reports:?}"
    );
    assert!(
        reports.iter().any(|r| r.starts_with("Landed {")),
        "no Landed report: {reports:?}"
    );
}

/// PageUp / E and PageDown / Q tilt the shot camera (the player's `look`),
/// within limits, and a new stroke starts from the camera's own view again.
#[test]
fn the_look_keys_tilt_the_shot_camera() {
    use xui_golf::fly::Action;
    use xui_golf::play::{LOOK_DOWN, LOOK_UP};

    let mut game = Game::with_params(xui_golf::Params::quick(4), 96);
    let (w, h) = (640, 360);
    let mut now = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(120);
    while game.run.is_none() {
        assert!(Instant::now() < deadline, "generation did not finish");
        game.tick(w, h, now);
        std::thread::sleep(Duration::from_millis(5));
    }
    now += Duration::from_millis(20);
    game.tick(w, h, now);
    let pitch0 = game.run.as_ref().unwrap().camera().pitch;
    assert_eq!(game.run.as_ref().unwrap().play.look, 0.0);

    game.held.press(Action::Up);
    for _ in 0..8 {
        now += Duration::from_millis(100);
        game.tick(w, h, now);
    }
    game.held.release(Action::Up);
    let run = game.run.as_ref().unwrap();
    assert!(run.play.look > 0.3, "look {}", run.play.look);
    assert!(
        run.camera().pitch > pitch0 + 0.25,
        "the camera tilted up: {pitch0} -> {}",
        run.camera().pitch
    );

    // Held for ever, it stops at the limit; down goes back past zero.
    game.held.press(Action::Up);
    for _ in 0..40 {
        now += Duration::from_millis(100);
        game.tick(w, h, now);
    }
    game.held.release(Action::Up);
    assert!((game.run.as_ref().unwrap().play.look - LOOK_UP).abs() < 1e-4);
    game.held.press(Action::Down);
    for _ in 0..100 {
        now += Duration::from_millis(100);
        game.tick(w, h, now);
    }
    game.held.release(Action::Down);
    assert!((game.run.as_ref().unwrap().play.look - LOOK_DOWN).abs() < 1e-4);

    // A new stroke starts from the camera's own view.
    let run = game.run.as_mut().unwrap();
    run.start_hole(1);
    assert_eq!(run.play.look, 0.0);
}

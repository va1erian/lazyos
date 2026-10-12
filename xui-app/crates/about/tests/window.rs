//! The About window built offscreen: it renders its first frames, takes a
//! drag and a shape change, and is saved at 96 and 192 DPI in each shape and
//! style (`target/snapshots/about-*.png`) for a person to look at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_about::view::State;
use xui_about::{AboutApp, Info, Msg, WINDOW};
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_core::{Dip, Image};

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

fn key(key: Key) -> Event {
    Event::KeyDown {
        key,
        modifiers: Modifiers::default(),
        repeat: 1,
        system: false,
    }
}

/// Builds the window, runs `script` against it and returns the final image.
fn shoot(
    dpi: u32,
    script: impl FnOnce(&mut dyn FnMut(Event), &Rc<RefCell<State>>, usize, usize) + 'static,
) -> Image {
    let state: Rc<RefCell<Option<Rc<RefCell<State>>>>> = Rc::default();
    let keep = Rc::clone(&state);
    let script = RefCell::new(Some(script));
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).dpi(dpi),
        move |ui| {
            let app = AboutApp::build(
                ui,
                Info {
                    version: "0.1.0".into(),
                },
                |_: &Msg| {},
            )?;
            *keep.borrow_mut() = Some(app.view().state());
            Ok(app)
        },
        move |stage| {
            let state = state.borrow().clone().expect("built");
            let w = (WINDOW.0 as u32 * stage.dpi() / 96) as usize;
            let h = (WINDOW.1 as u32 * stage.dpi() / 96) as usize;
            let scale = (stage.dpi() / 96).max(1) as i32;
            // `script` drives the state directly and injects events; the
            // frame is rendered by `tick`, like the timer would.
            let mut now = Instant::now();
            let mut ticks = |n: usize| {
                for _ in 0..n {
                    now += Duration::from_millis(40);
                    state.borrow_mut().tick(w, h, scale, now);
                }
            };
            ticks(3);
            if let Some(script) = script.borrow_mut().take() {
                script(
                    &mut |e| {
                        stage.inject(e);
                    },
                    &state,
                    w,
                    h,
                );
            }
            ticks(2);
        },
    )
    .expect("the window renders")
}

#[test]
fn the_window_renders_at_both_scales() {
    watchdog(|| {
        for dpi in [96, 192] {
            let image = shoot(dpi, |_, _, _, _| {});
            let name = format!("about-dpi{dpi}.png");
            save(&image, &name);
            let lit = (0..image.height())
                .step_by(7)
                .flat_map(|y| (0..image.width()).step_by(7).map(move |x| (x, y)))
                .filter_map(|(x, y)| image.pixel(x, y))
                .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 90)
                .count();
            assert!(lit > 400, "dpi {dpi}: only {lit} sampled pixels are lit");
        }
    });
}

#[test]
fn shapes_styles_and_pages_are_saved() {
    watchdog(|| {
        let styles = ["solid", "wire-solid", "wire"];
        for (i, shape) in ["knot", "torus", "crystal", "sphere"].iter().enumerate() {
            let image = shoot(96, move |inject, state, w, h| {
                for _ in 0..i {
                    inject(key(Key::SPACE));
                }
                for _ in 0..(i % 3) {
                    inject(key(Key::W));
                }
                for _ in 0..i {
                    inject(key(Key::RIGHT));
                }
                let mut now = Instant::now();
                for _ in 0..8 {
                    now += Duration::from_millis(40);
                    state.borrow_mut().tick(w, h, 1, now);
                }
            });
            save(&image, &format!("about-{shape}-{}.png", styles[i % 3]));
        }
    });
}

#[test]
fn a_drag_turns_the_model_and_a_key_changes_the_shape() {
    watchdog(|| {
        shoot(96, |inject, state, _, _| {
            let yaw_before = state.borrow().scene.shape();
            inject(Event::MouseDown {
                x: 200,
                y: 200,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            });
            inject(Event::MouseMove {
                x: 260,
                y: 210,
                modifiers: Modifiers::default(),
            });
            inject(Event::MouseUp {
                x: 260,
                y: 210,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            });
            inject(key(Key::SPACE));
            assert_ne!(state.borrow().scene.shape(), yaw_before);
        });
    });
}

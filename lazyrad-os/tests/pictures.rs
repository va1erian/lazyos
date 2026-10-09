//! The Picture Viewer sample (`samples/pictures`) run for real on the host:
//! the project is loaded as the player loads it, its window runs on xui's
//! offscreen backend, and the test pages, zooms, rotates and runs the slide
//! show through the same events the toolbar, keys and wheel raise.
//!
//! The viewer reads `app.documents`, which comes from the installed platform;
//! the platform is process-wide, so a test platform reads the documents of
//! the test that holds [`LOCK`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use lazyrad_os::platform::{Home, LazyOsPlatform};
use lazyrad_runtime::platform::Platform;
use lazyrad_runtime::{FormRuntime, Msg};
use xui_canvas::OffscreenBackend;
use xui_core::app::{run_app, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::image::Image;
use xui_core::units::Dip;
use xui_form::{LiveForm, Value};

/// Serialises the tests: each sets the documents the platform reports.
static LOCK: Mutex<()> = Mutex::new(());
/// The documents the test platform reports.
static DOCUMENTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A platform that reports [`DOCUMENTS`] and otherwise keeps the defaults
/// (an unrestricted policy: the sandbox has tests of its own).
struct TestPlatform;

impl Platform for TestPlatform {
    fn name(&self) -> &'static str {
        "test"
    }

    fn documents(&self) -> Vec<PathBuf> {
        DOCUMENTS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Holds [`LOCK`] with `documents` reported, installing the platform once.
fn with_documents(documents: Vec<PathBuf>) -> MutexGuard<'static, ()> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    INSTALLED.get_or_init(|| {
        let _ = lazyrad_runtime::platform::install(Box::new(TestPlatform));
    });
    *DOCUMENTS.lock().unwrap_or_else(|e| e.into_inner()) = documents;
    guard
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pictures")
}

fn event(control: &str, event: &str, args: Vec<Value>) -> Msg {
    Msg::Event {
        form: "main_form".to_owned(),
        control: control.to_owned(),
        event: event.to_owned(),
        args,
    }
}

fn click(control: &str) -> Msg {
    event(control, "Click", Vec::new())
}

fn key(name: &str) -> Msg {
    event("picture1", "KeyDown", vec![Value::Text(name.to_owned())])
}

fn wheel(delta: f64, ctrl: bool) -> Msg {
    event(
        "picture1",
        "Wheel",
        vec![Value::Float(delta), Value::Bool(ctrl)],
    )
}

/// A folder of pictures: two PNGs, a BMP (sorted between them, ignoring
/// case), a damaged "PNG" and a text file the viewer must skip.
fn picture_folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyos-pictures-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.png"), png(40, 20)).unwrap();
    std::fs::write(dir.join("B.bmp"), bmp(10, 10)).unwrap();
    std::fs::write(dir.join("broken.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    std::fs::write(dir.join("c.png"), png(1840, 540)).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a picture").unwrap();
    dir
}

fn png(w: u32, h: u32) -> Vec<u8> {
    Image::from_rgba(w, h, vec![0x80; (w * h * 4) as usize])
        .unwrap()
        .encode_png()
        .unwrap()
}

/// A 24-bit bottom-up BMP of one grey.
fn bmp(w: u32, h: u32) -> Vec<u8> {
    let stride = (w * 3).div_ceil(4) * 4;
    let mut file = b"BM".to_vec();
    file.extend_from_slice(&(54 + stride * h).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&(w as i32).to_le_bytes());
    file.extend_from_slice(&(h as i32).to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    file.extend(std::iter::repeat_n(0x60, (stride * h) as usize));
    file
}

/// Debug-build Rhai needs a deep stack for a handler a few calls down.
const STACK_BYTES: usize = 64 << 20;

/// Runs the viewer started with `documents`, delivers `messages`, and hands
/// the live form to `check`. Every handler must run without error.
///
/// `Msg` carries Rhai function pointers, which stay on one thread, so the
/// messages are built on the test's own thread.
fn run(
    documents: Vec<PathBuf>,
    messages: impl FnOnce() -> Vec<Msg> + Send + 'static,
    check: impl FnOnce(&LiveForm<Msg>) + Send + 'static,
) {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _guard = with_documents(documents);
            let messages = messages();
            let runtime = FormRuntime::load(sample()).expect("the sample loads");
            let errors = Rc::new(std::cell::RefCell::new(Vec::new()));
            let log = Rc::clone(&errors);
            runtime.set_error_observer(Rc::new(move |_form, control, event, error| {
                log.borrow_mut()
                    .push(format!("{control}_{event}: {error:?}"));
            }));
            let backend = Rc::new(OffscreenBackend::new());
            let seen = Rc::new(std::cell::RefCell::new(None));
            let slot = Rc::clone(&seen);
            let spec = PlatformSpec::new("pictures").size(Dip(920.0), Dip(620.0));
            run_app(backend as Rc<dyn Backend>, spec, move |ui: &mut Ui<Msg>| {
                let app = runtime
                    .build_app(ui, "main_form")
                    .expect("main_form builds");
                for message in messages {
                    ui.emit(message);
                }
                *slot.borrow_mut() = Some(app.root_form().expect("live").clone());
                app
            })
            .expect("the event loop runs");
            assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
            let form = seen.borrow_mut().take().expect("built");
            check(&form)
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn text(form: &LiveForm<Msg>, control: &str, property: &str) -> String {
    match form.get(control, property) {
        Some(Value::Text(text)) => text,
        other => panic!("{control}.{property}: {other:?}"),
    }
}

fn status(form: &LiveForm<Msg>) -> String {
    text(form, "status_label", "text")
}

#[test]
fn it_opens_the_picture_it_was_started_with() {
    let dir = picture_folder("start");
    run(vec![dir.join("B.bmp")], Vec::new, |form| {
        // a.png, B.bmp, broken.png, c.png: notes.txt is skipped.
        assert_eq!(status(form), "B.bmp   10 x 10 BMP   2 of 4   100%");
        assert_eq!(form.get("picture1", "has_image"), Some(Value::Bool(true)));
    });
}

#[test]
fn previous_and_next_page_through_the_folder_and_wrap() {
    let dir = picture_folder("paging");
    let messages = || vec![click("next_button"), click("next_button")];
    run(vec![dir.join("B.bmp")], messages, |form| {
        // broken.png, then c.png: fitted to the 920 x 540 box at 50%.
        assert_eq!(status(form), "c.png   1840 x 540 PNG   4 of 4   50%");
    });
    let messages = || vec![key("right"), key("right"), key("right")];
    run(vec![dir.join("B.bmp")], messages, |form| {
        assert!(
            status(form).starts_with("a.png "),
            "wraps: {}",
            status(form)
        );
    });
    run(
        vec![dir.join("a.png")],
        || vec![wheel(1.0, false)],
        |form| {
            assert!(
                status(form).starts_with("c.png "),
                "wraps back: {}",
                status(form)
            );
        },
    );
    run(
        vec![dir.join("c.png")],
        || vec![key("home")],
        |form| {
            assert!(status(form).starts_with("a.png "), "{}", status(form));
        },
    );
}

#[test]
fn a_damaged_picture_is_reported_not_fatal() {
    let dir = picture_folder("broken");
    run(vec![dir.join("broken.png")], Vec::new, |form| {
        let status = status(form);
        assert!(
            status.starts_with("broken.png   3 of 4   Cannot show this picture:"),
            "{status}"
        );
        assert_eq!(form.get("picture1", "has_image"), Some(Value::Bool(false)));
    });
}

#[test]
fn zoom_rotate_and_fit_follow_the_toolbar_keys_and_wheel() {
    let dir = picture_folder("view");
    let messages = || {
        vec![
            click("actual_button"),
            click("zoom_in_button"),
            wheel(1.0, true),
            click("rotate_cw_button"),
            key("k"),
        ]
    };
    run(vec![dir.join("c.png")], messages, |form| {
        assert_eq!(form.get("picture1", "rotation"), Some(Value::Int(180)));
        assert_eq!(
            status(form),
            "c.png   1840 x 540 PNG   4 of 4   200%   turned 180 degrees"
        );
    });
    let messages = || {
        vec![
            key("1"),
            event(
                "picture1",
                "DoubleClick",
                vec![Value::Float(5.0), Value::Float(5.0)],
            ),
            key("l"),
        ]
    };
    run(vec![dir.join("c.png")], messages, |form| {
        assert_eq!(form.get("picture1", "fit"), Some(Value::Bool(true)));
        // Turned left: 540 x 1840 fitted into 540 high.
        assert!(
            status(form).ends_with("29%   turned 270 degrees"),
            "{}",
            status(form)
        );
    });
}

#[test]
fn the_slide_show_starts_advances_and_stops() {
    let dir = picture_folder("show");
    let messages = || {
        vec![
            click("slideshow_button"),
            event("slide_timer", "Tick", Vec::new()),
        ]
    };
    run(vec![dir.join("a.png")], messages, |form| {
        assert_eq!(form.get("slide_timer", "enabled"), Some(Value::Bool(true)));
        assert_eq!(text(form, "slideshow_button", "icon"), "pause");
        assert!(status(form).starts_with("B.bmp "), "{}", status(form));
    });
    let messages = || vec![key("f11"), key("escape")];
    run(vec![dir.join("a.png")], messages, |form| {
        assert_eq!(form.get("slide_timer", "enabled"), Some(Value::Bool(false)));
        assert_eq!(text(form, "slideshow_button", "icon"), "play");
    });
}

#[test]
fn without_a_picture_or_samples_it_asks_for_one() {
    // The source tree has no `samples/` folder; the package brings one.
    run(
        Vec::new(),
        || vec![click("next_button"), key("e")],
        |form| {
            assert_eq!(
                status(form),
                "No pictures here. Click Open... to choose one."
            );
        },
    );
}

#[test]
fn edit_without_an_editor_says_so() {
    // No Messenger on the host: `sys::mimed` fails, and the script says why.
    let dir = picture_folder("edit");
    run(
        vec![dir.join("a.png")],
        || vec![click("edit_button")],
        |form| {
            assert!(
                status(form).starts_with("No editor could open this picture:"),
                "{}",
                status(form)
            );
        },
    );
}

#[test]
fn the_packaged_viewer_may_ask_mimed_and_nothing_else() {
    let scripts: Vec<String> = ["main_form.rhai", "paths.rhai"]
        .iter()
        .map(|name| std::fs::read_to_string(sample().join(name)).unwrap())
        .collect();
    let scripts: Vec<&str> = scripts.iter().map(String::as_str).collect();
    let found = LazyOsPlatform::ide(
        Home::from_var(Some(OsStr::new("/home/user"))),
        Path::new("/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lazyrad.elf"),
    )
    .script_permissions(&scripts);
    assert_eq!(found.interfaces, ["os.lazy.input.v1", "os.lazy.mimed.v1"]);
    assert!(found.topics.is_empty());
}

/// The window as drawn with `document` open, at `dpi`, after `messages`.
fn render(document: PathBuf, dpi: u32, messages: fn() -> Vec<Msg>) -> Image {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _guard = with_documents(vec![document]);
            let runtime = FormRuntime::load(sample()).expect("the sample loads");
            let backend = Rc::new(OffscreenBackend::with_dpi(dpi));
            let inner = Rc::clone(&backend);
            let shot = Rc::new(std::cell::RefCell::new(None));
            let slot = Rc::clone(&shot);
            let spec = PlatformSpec::new("pictures").size(Dip(920.0), Dip(620.0));
            run_app(backend as Rc<dyn Backend>, spec, move |ui: &mut Ui<Msg>| {
                let app = runtime
                    .build_app(ui, "main_form")
                    .expect("main_form builds");
                for message in messages() {
                    ui.emit(message);
                }
                // Once the loop runs: deliver the messages, then draw.
                let window = ui.window();
                let backend = Rc::clone(&inner);
                inner.set_run_hook(move || {
                    backend.pump(window);
                    let image = backend.render(window).expect("renders");
                    *slot.borrow_mut() =
                        Some(Image::from_rgba(image.width, image.height, image.pixels).unwrap());
                });
                app
            })
            .expect("the event loop runs");
            let image = shot.borrow_mut().take().expect("rendered");
            image
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Saves `image` for a human to look at (`target/snapshots/`).
fn save(image: &Image, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn a_wallpaper_is_drawn_fitted_with_the_toolbar_below() {
    let wallpaper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/wallpapers/Aurora.jpg");
    let image = render(wallpaper.clone(), 96, Vec::new);
    save(&image, "pictures.png");
    assert_eq!(image.size(), (920, 620));
    // 2560 x 1440 fitted into 920 x 540 is 920 x 517.5: centred, white
    // bands of about 11 px above and below.
    let [r, g, b, _] = image.pixel(460, 3).unwrap();
    assert_eq!([r, g, b], [255, 255, 255], "the band above the picture");
    let [r, g, b, _] = image.pixel(460, 270).unwrap();
    assert_ne!([r, g, b], [255, 255, 255], "the picture itself");
    let rotated = render(wallpaper, 192, || vec![key("k")]);
    save(&rotated, "pictures-rotated-2x.png");
    assert_eq!(rotated.size(), (1840, 1240), "drawn at 2x");
    // Turned upright: 1440 x 2560 fitted into 540 high leaves the sides white.
    let [r, g, b, _] = rotated.pixel(100, 540).unwrap();
    assert_eq!([r, g, b], [255, 255, 255], "beside the turned picture");
}

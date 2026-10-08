//! The process: where emusic keeps its files, the window on `xuid`, the
//! sound through `audiod`, and the exit status.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use emusic_frontend_portable::{Host, run_on};
use emusic_lazyaudio::LazyBackend;
use emusic_player::backend::AudioBackend;
use emusic_ui::startup::Startup;
use emusic_ui::waker::WakerSlot;
use xui_app::backend::LazyOSBackend;
use xui_app::platform::argv;
use xui_core::backend::Backend;

use crate::audiod::Audiod;
use crate::markers::{MarkingBackend, Sink};
use crate::soundcheck;
use crate::{WINDOW_SIZE, data_dir};

/// The argument that runs the sound check instead of the app.
const SOUND_CHECK: &str = "--sound-check";

/// Runs emusic until its window closes, then exits the process.
pub fn run() -> ! {
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|arg| arg == SOUND_CHECK) {
        sound_check(args.get(at + 1).map(String::as_str));
    }
    let home = std::env::var("HOME").ok();
    let data = PathBuf::from(data_dir(home.as_deref()));
    if let Err(error) = std::fs::create_dir_all(&data) {
        println!("EMUSIC:DATA:FAIL:{error}");
    }
    // SAFETY: the process has one thread so far; nothing reads the
    // environment concurrently. emusic's library, config and thumbnail cache
    // read this to keep all their files in the package's own folder (the one
    // place its manifest may write).
    unsafe { std::env::set_var("EMUSIC_DATA_DIR", &data) };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("EMUSIC_LOG").unwrap_or_else(|_| "warn".into()),
        ))
        .with_ansi(false)
        .init();

    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("EMUSIC:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    backend.on_first_frame(|| println!("EMUSIC:UP:PASS"));

    let config_path = data.join("config.toml");
    let startup = Startup {
        config: emusic_ui::config::load(&config_path),
        config_path: Some(config_path),
        ipc: None,
        files: argv::file_arg(std::env::args_os()).into_iter().collect(),
        enqueue: false,
        waker: WakerSlot::new(),
        mock: false,
    };
    let serial: Sink = Arc::new(|line| println!("{line}"));
    let audio: Arc<dyn AudioBackend> = Arc::new(MarkingBackend::new(
        Arc::new(LazyBackend::new(Arc::new(Audiod))),
        serial,
    ));
    let host = Host {
        backend: Rc::clone(&backend) as Rc<dyn Backend>,
        // The compositor draws the frame and its buttons.
        native_chrome: true,
        label: "LazyOS xuid",
        size: Some(WINDOW_SIZE),
        audio: Some(audio),
    };
    let outcome = run_on(startup, host);
    backend.unbind();
    std::process::exit(i32::from(xui_app::launch::finish("EMUSIC", outcome)))
}

/// `--sound-check <file>`: plays `file` through `audiod` in the fixed
/// sequence `tools/emusic/run.py` records, then exits.
fn sound_check(track: Option<&str>) -> ! {
    // The Terminal shows one line: the details go to a log in the app folder.
    let home = std::env::var("HOME").ok();
    let data = PathBuf::from(data_dir(home.as_deref()));
    let _ = std::fs::create_dir_all(&data);
    if let Ok(log) = std::fs::File::create(data.join("sound-check.log")) {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn,lazyemusic=trace"))
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log))
            .init();
    }
    let serial: Sink = Arc::new(|line| {
        tracing::info!("{line}");
        println!("{line}");
    });
    let Some(track) = track else {
        println!("EMUSIC:CHECK:FAIL:no track");
        std::process::exit(2);
    };
    let backend = LazyBackend::new(Arc::new(Audiod));
    match soundcheck::run(&backend, std::path::Path::new(track), &serial, 1.0) {
        Ok(()) => {
            println!("EMUSIC:CHECK:DONE");
            std::process::exit(0)
        }
        Err(reason) => {
            println!("EMUSIC:CHECK:FAIL:{reason}");
            std::process::exit(1)
        }
    }
}

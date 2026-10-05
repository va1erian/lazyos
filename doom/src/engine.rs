//! Start-up and the main loop: parse the command line, check the IWAD, move
//! into the config directory, choose the backend, then hand control to the
//! engine and tick it forever (it exits the process itself, on quit or error).

use std::ffi::{c_char, c_int, CString};

use lazydoom::launch::{self, Mode};

use crate::headless::Headless;
use crate::hooks::{self, Backend};
use crate::window::Window;
use xui_app::client_window::OpenError;

extern "C" {
    fn doomgeneric_Create(argc: c_int, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
}

/// The window opens at the engine's own resolution (`DOOMGENERIC_RESX/Y`).
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 400;

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());
    let launch = launch::parse(&args, &cwd);
    // Opened, not stat'ed: what matters is that the engine can open it (and
    // LazyOS's `statx` has answered success for a missing path).
    if std::fs::File::open(&launch.iwad).is_err() {
        // Fail soft with one clear line: the engine's own error is a terse
        // "IWAD file not found" with no hint of where it looked.
        println!("DOOM:IWAD:FAIL {} is missing", launch.iwad);
        std::process::exit(1);
    }
    enter_config_dir();
    let backend = match launch.mode {
        Mode::Window => match Window::open(WIDTH, HEIGHT, "Freedoom") {
            Ok(window) => Backend::Window(window),
            // Closed before its first frame: the user dismissed it, not a failure.
            Err(OpenError::Closed) => {
                println!("DOOM:QUIT:PASS");
                std::process::exit(0);
            }
            Err(error) => {
                println!("DOOM:WINDOW:FAIL {error}");
                std::process::exit(1);
            }
        },
        Mode::Headless { frames } => Backend::Headless(Headless::new(frames)),
    };
    let mode = if matches!(backend, Backend::Window(_)) {
        "window"
    } else {
        "headless"
    };
    println!("DOOM:UP:PASS mode={mode} iwad={}", launch.iwad);
    hooks::install(backend);
    start(&launch.engine_args);
    loop {
        // SAFETY: the engine was created above and is only ever driven from
        // this thread.
        unsafe { doomgeneric_Tick() };
    }
}

/// `doomgeneric_Create` with a C argv. The strings are leaked on purpose: the
/// engine keeps `myargv` for the whole run (`M_CheckParm` reads it at any time).
fn start(args: &[String]) {
    let mut argv: Vec<*mut c_char> = args
        .iter()
        .filter_map(|arg| CString::new(arg.as_str()).ok())
        .map(CString::into_raw)
        .collect();
    let argc = argv.len() as c_int;
    argv.push(core::ptr::null_mut());
    let argv = Box::leak(argv.into_boxed_slice());
    // SAFETY: `argv` holds `argc` valid NUL-terminated strings followed by a
    // null, all leaked so they outlive the engine.
    unsafe { doomgeneric_Create(argc, argv.as_mut_ptr()) };
}

/// The engine writes `default.cfg` and `.savegame/` to its working directory;
/// make that a writable per-user directory. Best effort: on failure the engine
/// still runs, it just cannot save.
fn enter_config_dir() {
    let home = std::env::var("HOME").ok();
    let dir = launch::config_dir(home.as_deref());
    let fallback = launch::config_dir(None);
    for candidate in [dir, fallback] {
        if std::fs::create_dir_all(&candidate).is_ok()
            && std::env::set_current_dir(&candidate).is_ok()
        {
            return;
        }
    }
}

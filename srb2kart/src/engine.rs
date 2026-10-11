//! Start-up: parse the command line, check the game data, create the config
//! directory, choose the backend, then hand control to the engine, which runs
//! its own loop and ends the process itself (`lz_quit`).

use std::ffi::{c_char, c_int, CString};

use lazysrb2kart::backend::Backend;
use lazysrb2kart::headless::Headless;
use lazysrb2kart::launch::{self, Launch, Mode};

use crate::hooks;

extern "C" {
    fn srb2kart_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());
    let home = std::env::var("HOME").ok();
    let launch = launch::parse(&args, home.as_deref(), &cwd);
    check_data(&launch);
    create_home(&launch);
    let (backend, mode): (Box<dyn Backend>, &str) = match launch.mode {
        Mode::Headless { frames } => (Box::new(Headless::new(frames)), "headless"),
        Mode::Window => {
            println!("SRB2KART:WINDOW:FAIL the window is not implemented yet (K1); use -headless");
            std::process::exit(1);
        }
    };
    println!("SRB2KART:UP:PASS mode={mode} data={}", launch.data_dir);
    hooks::set_dirs(&launch.data_dir, &launch.home_dir);
    hooks::install(backend);
    let code = start(&launch.engine_args);
    std::process::exit(code);
}

/// Fail soft with one clear line: the engine's own error is a terse
/// "SRB2.SRB not found" with no hint of why. Opened, not stat'ed: what matters
/// is that the engine can open it.
fn check_data(launch: &Launch) {
    let iwad = format!("{}/{}", launch.data_dir.trim_end_matches('/'), launch::IWAD);
    if std::fs::File::open(&iwad).is_err() {
        println!("SRB2KART:DATA:FAIL {iwad} is missing");
        std::process::exit(1);
    }
}

/// The engine writes its config, saves and replays under its home; make it a
/// writable per-user directory. On failure the engine's own "couldn't write
/// game config" error follows.
fn create_home(launch: &Launch) {
    if let Err(error) = std::fs::create_dir_all(&launch.home_dir) {
        println!("SRB2KART:LOG: cannot create {}: {error}", launch.home_dir);
    }
}

/// `srb2kart_main` with a C argv. The strings are leaked on purpose: the
/// engine keeps `myargv` for the whole run (`M_CheckParm` reads it at any time).
fn start(args: &[String]) -> c_int {
    let mut argv: Vec<*mut c_char> = args
        .iter()
        .filter_map(|arg| CString::new(arg.as_str()).ok())
        .map(CString::into_raw)
        .collect();
    let argc = argv.len() as c_int;
    argv.push(core::ptr::null_mut());
    let argv = Box::leak(argv.into_boxed_slice());
    // SAFETY: `argv` holds `argc` valid NUL-terminated strings followed by a
    // null, all leaked so they outlive the engine; it is called once, from
    // this thread, after the hooks are installed.
    unsafe { srb2kart_main(argc, argv.as_mut_ptr()) }
}

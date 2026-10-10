//! The LazyOS half of the port: everything id's sys_win.c did for its
//! window, on `xuid` — the `xuid` window and its keys ([`window`]),
//! the record bridge the game runs over ([`bridge`]), the headless
//! harness ([`headless`]), and the platform logic host-testable anywhere:
//! the command line ([`launch`]), the Quake keynum map ([`keymap`]), the
//! session state ([`session`]), presentation ([`pixels`]), the frame
//! checksums ([`crc`]) and the record encoders ([`records`]).
//!
//! The platform decides two paths id's main never did: the search path
//! opens the package's read-only resources (`-basedir
//! <install>/resources`, the shareware `id1/pak0.pak`), and the data
//! directory (the saves and `config.cfg`) is the package's own folder in
//! the player's home (`/tmp/quake` with none), which [`launch`] derives
//! and `main` tells `common` about before the first frame.
//!
//! The windowed flow then is id's: open the window, connect the bridge,
//! and hand the engine its command line to `sys::run` — the whole
//! program; it returns only when the game quits (its `Quit` record, or
//! the window closing). At the end: `QUAKE:QUIT:PASS`, the surface down,
//! and the process ends the way id's `exit(0)` did.

pub mod crc;
pub mod headless;
pub mod keymap;
pub mod launch;
pub mod pixels;
pub mod records;
pub mod session;

// The `xuid` side only exists on the OS: a host build runs the engine's
// own tests over the record protocol, without a compositor to talk to.
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod bridge;
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod window;
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod window_input;

/// Open the desktop window and drive the game (windowed mode): the whole
/// program for the rest of the process's life.
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub fn run_window(launch: launch::Launch) -> std::process::ExitCode {
    let window = match window::Window::open(window::WIDTH, window::HEIGHT, "Quake") {
        Ok(window) => window,
        // Closed before its first frame: the user dismissed it, not a
        // failure.
        Err(window::OpenError::Closed) => {
            println!("QUAKE:QUIT:PASS");
            return std::process::ExitCode::SUCCESS;
        }
        Err(error) => {
            println!("QUAKE:WINDOW:FAIL {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("QUAKE:UP:PASS mode=window,preset=classic");
    sys_run(&launch, Some(window), None)
}

/// Drive the game headless: no window, frames checksummed to a verdict
/// ([`bridge::connect`]'s budget, [`headless`]).
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub fn run_headless(launch: launch::Launch, frames: Option<u32>) -> std::process::ExitCode {
    println!("QUAKE:UP:PASS mode=headless,preset=classic");
    sys_run(&launch, None, frames)
}

/// The two flows' common shape: the bridge connected and `sys::run` only
/// wrapping the wrapper's default preset made explicit, and the markers
/// on the way out.
#[cfg(all(target_os = "linux", target_env = "musl"))]
fn sys_run(
    launch: &launch::Launch,
    window: Option<window::Window>,
    frames: Option<u32>,
) -> std::process::ExitCode {
    let budget =
        if window.is_none() { Some(frames.unwrap_or(bridge::HEADLESS_DEFAULT_FRAMES)) } else { None };
    let (pump, sink) = bridge::connect(launch, window, budget);
    // The command line the program's own run walks: after `argv[0]`,
    // wrapper flags off (`launch::parse` did that), the launcher's preset
    // made explicit.
    let engine_args = launch::with_default_preset(launch.game_args.clone());
    let result = crate::sys::run(pump, sink, &engine_args[1..]);
    match result {
        Ok(()) => {
            println!("QUAKE:QUIT:PASS");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("quake: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

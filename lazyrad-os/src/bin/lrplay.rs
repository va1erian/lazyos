//! `lrplay`: the LazyRAD player on LazyOS.
//!
//! A `xuid` desktop client (or the display owner, for a headless CI session)
//! that runs one LazyRAD project: it is the program `init`/`mimed` start for a
//! project and the stub inside every `.lzp` the packager produces
//! (docs/lazyrad-plan.md, P1/P2). It is a thin shell around
//! `lazyrad_player::run_with_backend`; the window comes from
//! `xui_app::backend::LazyOSBackend`, the window title from the form's own
//! `title` (the backend forwards it as `SetTitle`), and closing the window
//! ends the event loop.
//!
//! Command line: see `lazyrad_os::args`. Serial evidence:
//! `LRPLAY:DATA:PASS:<dir>` naming the scripts' read/write folder (in the
//! user's home; `LRPLAY:HOME:WARN` first when `$HOME` is unset),
//! `LRPLAY:MSG:PASS` once form scripts have the `msg` and `sys::*` modules
//! (Messenger), `LRPLAY:MSGEVENT:PASS` after the first Messenger handler (a
//! topic event, a call to a service the script serves) ran without error,
//! `LRPLAY:UP:PASS` after the first frame reached the compositor,
//! `LRPLAY:EVENT:PASS` after the first script event handler ran,
//! `LRPLAY:EXIT:PASS` after the loop ended cleanly,
//! `LRPLAY:MODPLAY:PASS:title=.. audio=0|1 rate=..` when a script starts a
//! song (`LRPLAY:MODPLAY:FAIL:<why>` when its sound stops with an error),
//! `LRPLAY:MODEND:PASS:elapsed_ms=..` when one has played out, and
//! `LRPLAY:<STAGE>:FAIL:<why>` (`ARGS`, `BIND`, `RUN`) otherwise.
//!
//! A failed run also tells `init` why (`init.ReportFailure`, issue #549):
//! the last problem the player reported on stderr, so the desktop's "app
//! stopped" notice shows it (`LRPLAY:REASON:PASS:<text>` on serial).

use std::process::ExitCode;
use std::rc::Rc;

use lazyrad_os::args;
use lazyrad_os::failure::{report_to_init, StderrTap};
use lazyrad_os::marker::Markers;
use lazyrad_os::platform::{data_root, player_policy, Home, LazyOsPlatform};
use lazyrad_player::{run_with_backend, EXIT_OK};
use xui_app::backend::LazyOSBackend;
use xui_core::backend::Backend;

const MARK: Markers = Markers::PLAYER;

fn main() -> ExitCode {
    MARK.install_panic_hook();
    // Remember the last problem the player reports, for `init` (#549).
    let tap = StderrTap::install();
    let parsed = match args::parse_player(std::env::args_os().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => return fail("ARGS", &error.to_string()),
    };
    // Not `current_exe()`: the kernel answers `/busybox` (see `lazyrad_os::args`).
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let exe = args::exe_from_argv0(std::env::args_os().next().as_deref(), &cwd);
    let project = match args::resolve_project(parsed.project.as_deref(), &exe, |p| p.exists()) {
        Ok(project) => project,
        Err(error) => return fail("ARGS", &error.to_string()),
    };

    MARK.pass_with(
        "PROJECT",
        &format!("{} exe={}", project.display(), exe.display()),
    );
    let home = Home::from_env();
    if let Some(warning) = home.fallback_warning() {
        MARK.warn("HOME", &warning);
    }
    let policy = player_policy(&exe, &project, &home);
    // Best effort: an unwritable home only means `file_write_text` reports an
    // error to the script.
    let data = data_root(&exe, &home);
    let _ = std::fs::create_dir_all(&data);
    MARK.pass_with("DATA", &data.to_string_lossy());
    if lazyrad_runtime::platform::install(Box::new(LazyOsPlatform::player(policy, home))).is_err() {
        return fail("ARGS", "a platform was already installed");
    }

    // Messenger for form scripts, before any engine is built.
    if lazyrad_os::messenger::install(Some(Rc::new(MARK.once("MSGEVENT")))) {
        MARK.pass("MSG");
    }
    // ProTracker songs for form scripts (`modplay::*`), through the mixer.
    lazyrad_os::tracker::install(Some(Rc::new(|stage: &str, detail: &str| {
        match stage.strip_suffix(":FAIL") {
            Some(stage) => MARK.fail(stage, detail),
            None => MARK.pass_with(stage, detail),
        }
    })));

    // Monospace next to the UI face, before the backend exists: the shaper
    // builds its font database once.
    xui_app::font::register_mono();
    let first_event = Rc::new(MARK.once("EVENT"));
    let code = run_with_backend(&[project.to_string_lossy().into_owned()], &mut |runtime| {
        let backend = LazyOSBackend::connect().map_err(|code| {
            MARK.fail("BIND", &format!("code {code}"));
            format!("cannot reach the display (error {code})")
        })?;
        backend.on_first_frame(|| MARK.pass("UP"));
        lazyrad_os::desktop_mode::set_theme(backend.desktop_theme());
        if let Some(runtime) = runtime {
            lazyrad_os::probe::print_startup_form(runtime, backend.scale() as i32);
            let seen = Rc::clone(&first_event);
            runtime.set_handler_observer(Rc::new(move |_form, _control, _event| seen()));
        }
        let backend = Rc::new(backend);
        Ok(backend as Rc<dyn Backend>)
    });
    report(code, tap)
}

/// Prints the exit marker and converts the player's code to a process status.
fn report(code: i32, tap: Option<StderrTap>) -> ExitCode {
    let reason = tap.and_then(StderrTap::finish);
    if code == EXIT_OK {
        MARK.pass("EXIT");
        ExitCode::SUCCESS
    } else {
        MARK.fail("RUN", &format!("exit code {code}"));
        tell_init(
            reason
                .as_deref()
                .unwrap_or("the program stopped with an error"),
        );
        ExitCode::from(u8::try_from(code).unwrap_or(1))
    }
}

/// Hand `init` the reason this run fails, and log it.
fn tell_init(reason: &str) {
    MARK.pass_with("REASON", reason);
    report_to_init(reason);
}

/// Reports a start-up failure on serial and stderr and exits non-zero.
fn fail(stage: &str, why: &str) -> ExitCode {
    MARK.fail(stage, why);
    eprintln!("lrplay: {why}");
    tell_init(why);
    ExitCode::FAILURE
}

//! `lazyrad --play-dev <project>`: one development run without a window
//! (issue #529).
//!
//! The same path as Play in a packaged IDE ([`crate::devplay`]), driven by a
//! plain loop instead of the window's timer, so a session script can start it
//! through `init` (as the installed IDE, under its own label) and judge the
//! result from serial: `LRIDE:PLAY:OUT:<line>` for every line the player
//! printed or the run reported, `LRIDE:PLAY:EXIT:<code>` (`none` when the
//! player did not exit normally), or `LRIDE:PLAY:FAIL:<why>` when the run did
//! not start.

use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use lazyrad_ide::run::{EventSink, Launcher, RunEvent};

use crate::launcher::PollingLauncher;

/// The launcher for this IDE: [`crate::devplay::DevLauncher`] when it runs
/// under a package label (so a plain fork would hand the project the IDE's
/// permissions), [`PollingLauncher`] otherwise.
pub fn launcher_for_this_process(author: &str) -> Rc<dyn Launcher> {
    #[cfg(unix)]
    if xui_app::sys::cred_get(None).is_ok_and(|cred| cred.label_id != 0) {
        return Rc::new(crate::devplay::DevLauncher::new(author));
    }
    let _ = author;
    Rc::new(PollingLauncher)
}

/// How often the loop polls the run, in milliseconds (the IDE's timer).
const POLL_MS: u64 = 100;

/// Run `project_dir` once under its development label with `player` (the
/// `lrplay` beside the IDE, [`crate::platform::player_beside`]); returns the
/// player's exit code (1 when it did not exit normally or never started).
pub fn play_headless(player: &Path, project_dir: &Path, author: &str) -> i32 {
    let exit: Arc<Mutex<Option<Option<i32>>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&exit);
    let sink: EventSink = Arc::new(move |_run, event| match event {
        RunEvent::Output(line) => println!("LRIDE:PLAY:OUT:{line}"),
        RunEvent::Diagnostic(report) => println!("LRIDE:PLAY:OUT:error: {}", report.message),
        RunEvent::Exited(code) => {
            if let Ok(mut slot) = seen.lock() {
                *slot = Some(code);
            }
        }
    });
    let launcher = launcher_for_this_process(author);
    let mut child = match launcher.launch(player, project_dir, 1, sink) {
        Ok(child) => child,
        Err(error) => {
            println!("LRIDE:PLAY:FAIL:{error}");
            return 1;
        }
    };
    while child.is_running() {
        child.poll();
        xui_app::sys::sleep_millis(POLL_MS);
    }
    let code = exit.lock().ok().and_then(|slot| *slot).flatten();
    match code {
        Some(code) => println!("LRIDE:PLAY:EXIT:{code}"),
        None => println!("LRIDE:PLAY:EXIT:none"),
    }
    code.unwrap_or(1)
}

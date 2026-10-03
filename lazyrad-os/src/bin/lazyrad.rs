//! `lazyrad`: the LazyRAD IDE on LazyOS.
//!
//! A `xuid` desktop client running the portable `lazyrad-ide` on
//! `xui_app::backend::LazyOSBackend`, with the LazyOS platform installed
//! (docs/lazyrad-plan.md, P3). Serial evidence: `LRIDE:HOME:PASS:<home>` (or
//! `LRIDE:HOME:WARN` when `$HOME` is unset), `LRIDE:UP:PASS` after the first
//! frame reached the compositor, `LRIDE:OPEN:PASS:<project>` when a project
//! opened, `LRIDE:RUN:PASS:<project>` when Run started the player and
//! `LRIDE:CHILD:PASS:exit=<code>` when it ended, `LRIDE:EXIT:PASS` after a clean
//! exit, `LRIDE:<STAGE>:FAIL:<why>` otherwise.
//!
//! Run packaged (under an `app:` label), Play runs the project under its own
//! development label (`lazyrad_os::devplay`). `--play-dev <project>` does one
//! such run without a window and exits with the player's status, printing
//! `LRIDE:PLAY:OUT:<line>` per line and `LRIDE:PLAY:EXIT:<code>`: what a
//! session script drives.

use std::process::ExitCode;
use std::rc::Rc;

use std::path::PathBuf;

use lazyrad_ide::{IdeEvent, RunOptions};
use lazyrad_os::args;
use lazyrad_os::marker::Markers;
use lazyrad_os::pkgd::PkgdInstaller;
use lazyrad_os::platform::{Home, LazyOsPlatform};
use xui_app::backend::LazyOSBackend;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;

const MARK: Markers = Markers::IDE;
/// The flag of a windowless development run.
const PLAY_DEV: &str = "--play-dev";

/// The project folder to open, from `[<dir | .lrp>]` on the command line (the
/// launcher's `--client` and `attempt=N` are accepted and ignored). A `.lrp`
/// file means its folder; a relative path is taken from the working directory.
fn project_to_open() -> Result<Option<PathBuf>, String> {
    let args = std::env::args_os().skip(1).filter(|arg| arg != PLAY_DEV);
    let parsed = args::parse_player(args).map_err(|e| e.to_string())?;
    let Some(path) = parsed.project else {
        return Ok(None);
    };
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let full = if path.to_string_lossy().starts_with('/') || path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let is_lrp = full.extension().is_some_and(|ext| ext == "lrp");
    Ok(Some(match full.parent() {
        Some(parent) if is_lrp => parent.to_path_buf(),
        _ => full,
    }))
}

fn main() -> ExitCode {
    MARK.install_panic_hook();
    let open = match project_to_open() {
        Ok(open) => open,
        Err(why) => {
            MARK.fail("ARGS", &why);
            return ExitCode::FAILURE;
        }
    };
    let home = Home::from_env();
    if let Some(warning) = home.fallback_warning() {
        MARK.warn("HOME", &warning);
    }
    // The projects folder is the dialog's first stop and the designer
    // preview's sandbox; a fresh home does not have it yet. Best effort: an
    // unwritable home only means the dialog starts in the home instead.
    let _ = std::fs::create_dir_all(home.projects_dir());
    MARK.pass_with("HOME", &home.path().to_string_lossy());
    if lazyrad_runtime::platform::install(Box::new(LazyOsPlatform::ide(home))).is_err() {
        MARK.fail("PLATFORM", "a platform was already installed");
        return ExitCode::FAILURE;
    }
    let author = std::env::var("USER").unwrap_or_else(|_| "lazyos".to_owned());
    if std::env::args_os().any(|arg| arg == PLAY_DEV) {
        let Some(project) = open else {
            MARK.fail("ARGS", "--play-dev needs a project folder");
            return ExitCode::FAILURE;
        };
        let code = lazyrad_os::playdev::play_headless(&project, &author);
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    // The code editor needs a real monospace face next to the UI face; register
    // before the backend exists (the shaper builds its font database once).
    xui_app::font::register_mono();
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => backend,
        Err(code) => {
            MARK.fail("BIND", &format!("code {code}"));
            return ExitCode::FAILURE;
        }
    };
    // Resizable and maximizable, like the other desktop apps.
    backend.set_size_hints(640, 420, 0, 0);
    backend.on_first_frame(|| MARK.pass("UP"));
    // `xuid` bounds a surface to the screen (1280x720 in the screenshot
    // sessions); leave room for the title bar and the taskbar.
    let spec = PlatformSpec::new("LazyRAD").size(Dip(1100.0), Dip(600.0));
    let observer: lazyrad_ide::IdeObserver = Rc::new(|event| match event {
        IdeEvent::ProjectOpened(name) => MARK.pass_with("OPEN", name),
        IdeEvent::RunStarted(name) => MARK.pass_with("RUN", name),
        IdeEvent::RunExited(code) => MARK.pass_with("CHILD", &format!("exit={code:?}")),
        IdeEvent::PackageReviewed(id, n) => {
            MARK.pass_with("PKG:REVIEW", &format!("{id} permissions={n}"))
        }
        IdeEvent::PackageInstalled(id, true) => MARK.pass_with("PKG:INSTALL", id),
        IdeEvent::PackageInstalled(id, false) => MARK.pass_with("PKG:SAVED", id),
        IdeEvent::PackageFailed(why) => MARK.fail("PKG:INSTALL", why),
        IdeEvent::AppLaunched(id) => MARK.pass_with("PKG:LAUNCH", id),
        IdeEvent::DocumentEdited(name, chars) => MARK.pass_with("EDIT", &format!("{name} {chars}")),
    });
    let options = RunOptions {
        spec,
        open,
        observer: Some(observer),
        launcher: Some(lazyrad_os::playdev::launcher_for_this_process(&author)),
        installer: Some(Rc::new(PkgdInstaller::on_lazyos())),
        author,
    };
    match lazyrad_ide::run_with_options(Rc::new(backend) as Rc<dyn Backend>, options) {
        Ok(()) => {
            MARK.pass("EXIT");
            ExitCode::SUCCESS
        }
        Err(error) => {
            MARK.fail("RUN", &error.to_string());
            ExitCode::FAILURE
        }
    }
}

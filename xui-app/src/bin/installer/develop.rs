//! The Installer's `develop` verb (issue #529,
//! `docs/lazyrad-package-plan.md` section 3): approve a development run.
//!
//! An IDE that is itself a package cannot call `pkgd` (labelled callers are
//! refused), so to run a project under the project's own permissions it writes
//! the project's `.lzp` to `/transient` and asks `mimed` to open it with the
//! verb `develop`. `mimed` routes that to this program, which `init` starts
//! unlabelled in the caller's session, with `--develop <path>`:
//!
//! 1. `pkgd.Develop(path, confirm = false)`: when this session already
//!    approved the same or a wider rule set, `pkgd` loads it and nothing is
//!    shown (`INSTALLER:DEVELOP:PASS <label> asked=0`);
//! 2. otherwise one window, the consent: "Run <app> from your development
//!    environment with these permissions", every permission grouped by risk as
//!    `Inspect` explains it, and **Allow** / **Cancel**. Allow calls
//!    `Develop(path, confirm = true)` (`INSTALLER:DEVELOP:PASS <label> asked=1`);
//!    Cancel, Esc or the close button refuse: `pkgd.DevelopDeclined` publishes
//!    the refusal the IDE waits on (`INSTALLER:DEVELOP:DENIED`).
//!
//! Like every Installer screen, the window takes nothing from the IDE but the
//! path: what it shows comes from `pkgd`, which re-reads the package itself.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::installer::{clean, elide, Package};
use xui_app::platform::{argv, pkg};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_core::widget::{Button, Label, ListView, Panel};
use xui_core::Key;

use crate::consent::permission_items;
use crate::view::{fail, rect, MARGIN};

/// The flag `init`'s `installer-develop` row passes before the path.
pub const FLAG: &str = "--develop";

/// The package path after [`FLAG`], when the Installer was started for the
/// `develop` verb. A missing or unusable path is `Some(None)`: the verb was
/// asked for, so the Installer must not fall back to its install wizard.
pub fn requested<I: IntoIterator<Item = OsString>>(args: I) -> Option<Option<PathBuf>> {
    let mut args = args.into_iter().skip_while(|arg| arg != FLAG);
    args.next()?;
    Some(argv::path_arg(args.next()))
}

/// Run the `develop` verb for `path` and exit.
pub fn main(path: Option<PathBuf>) -> ExitCode {
    let Some(path) = path.filter(|path| argv::is_acceptable(path)) else {
        println!("INSTALLER:DEVELOP:FAIL not an absolute path");
        return ExitCode::FAILURE;
    };
    let text = path.to_string_lossy().into_owned();
    match pkg::develop(&text, false) {
        Ok((label, true)) => {
            println!("INSTALLER:DEVELOP:PASS {} asked=0", clean(&label));
            return ExitCode::SUCCESS;
        }
        Ok((_, false)) => {}
        Err(reason) => {
            println!("INSTALLER:DEVELOP:FAIL {}", clean(&reason));
            return ExitCode::FAILURE;
        }
    }
    let package = match pkg::inspect(&text) {
        Ok(package) => package,
        Err(reason) => {
            println!("INSTALLER:DEVELOP:FAIL {}", clean(&reason));
            return ExitCode::FAILURE;
        }
    };
    ask(&path, package)
}

/// The consent window's messages.
#[derive(Clone, Debug)]
pub enum DevMsg {
    Allow,
    Deny,
}

/// The consent window: the package, its permissions and the two buttons.
struct Consent {
    path: String,
    _panel: Panel<DevMsg>,
    _labels: Vec<Label<DevMsg>>,
    _list: ListView<DevMsg>,
    _buttons: Vec<Button<DevMsg>>,
}

impl App for Consent {
    type Msg = DevMsg;

    fn update(&mut self, msg: DevMsg, ui: &mut Ui<DevMsg>) {
        match msg {
            DevMsg::Allow => match pkg::develop(&self.path, true) {
                Ok((label, true)) => println!("INSTALLER:DEVELOP:PASS {} asked=1", clean(&label)),
                Ok((label, false)) => {
                    println!("INSTALLER:DEVELOP:FAIL not approved {}", clean(&label))
                }
                Err(reason) => println!("INSTALLER:DEVELOP:FAIL {}", clean(&reason)),
            },
            DevMsg::Deny => {
                // Tell pkgd, so the IDE waiting for the answer stops now.
                if let Err(reason) = pkg::develop_declined(&self.path) {
                    println!("INSTALLER:DEVELOP:FAIL {}", clean(&reason));
                }
                println!("INSTALLER:DEVELOP:DENIED");
            }
        }
        ui.quit();
    }
}

impl Consent {
    fn build(ui: &mut Ui<DevMsg>, path: &Path, package: &Package) -> Result<Consent, String> {
        let bounds = ui.client_rect();
        let (width, height) = (bounds.width().max(360), bounds.height().max(260));
        let inner = width - 2 * MARGIN;
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let name = elide(&package.name, 60);
        let lines = [
            (24, format!("Run {name} from your development environment?")),
            (
                16,
                format!(
                    "{} {} by {} (unverified) will run with these permissions until you log out.",
                    elide(&package.system_name, 60),
                    elide(&package.version, 20),
                    elide(&package.author, 40)
                ),
            ),
            (
                16,
                "Allow only if you trust the project you are running.".to_owned(),
            ),
        ];
        let mut labels = Vec::new();
        let mut top = MARGIN;
        for (line_h, text) in lines {
            labels.push(
                Label::new(page, rect(MARGIN, top, inner, line_h), &elide(&text, 140))
                    .map_err(fail)?,
            );
            top += line_h + 6;
        }
        let list_h = (height - top - 60).max(40);
        let items = permission_items(package);
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let list = ListView::new(page, rect(MARGIN, top, inner, list_h), &refs).map_err(fail)?;
        list.select(None);
        let allow = Button::new(
            page,
            rect(width - MARGIN - 228, height - 48, 100, 30),
            "Allow",
        )
        .map_err(fail)?
        .on_click(|| Some(DevMsg::Allow));
        let deny = Button::new(
            page,
            rect(width - MARGIN - 116, height - 48, 100, 30),
            "Cancel",
        )
        .map_err(fail)?
        .on_click(|| Some(DevMsg::Deny));
        // Cancel has the focus: Enter alone never consents.
        page.focus(deny.id());
        ui.on_close(|| Some(DevMsg::Deny));
        ui.on_key(|key, _| (key == Key::ESCAPE).then_some(DevMsg::Deny));
        Ok(Consent {
            path: path.to_string_lossy().into_owned(),
            _panel: panel,
            _labels: labels,
            _list: list,
            _buttons: vec![allow, deny],
        })
    }
}

/// Show the consent window for `package` and act on the answer.
fn ask(path: &Path, package: Package) -> ExitCode {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("INSTALLER:BIND:FAIL:{code}");
            return ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size((560, 400));
    backend.set_size_hints(420, 300, 0, 0);
    let permissions = package.permissions.len();
    backend.on_first_frame(move || println!("INSTALLER:DEVELOP:ASK perms={permissions}"));
    let theme = backend.desktop_theme();
    let spec =
        PlatformSpec::new("Run from development").size(Dip(width as f32), Dip(height as f32));
    let path = path.to_path_buf();
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        match Consent::build(ui, &path, &package) {
            Ok(app) => app,
            Err(error) => {
                println!("INSTALLER:BUILD:FAIL:{error}");
                std::process::exit(1);
            }
        }
    });
    backend.unbind();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("INSTALLER:RUN:FAIL:{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_flag_selects_the_verb_and_takes_the_next_argument() {
        let absolute = std::env::current_dir().unwrap().join("p.lzp");
        let text = absolute.to_string_lossy().into_owned();
        assert_eq!(
            requested(args(&["installer", "--client", FLAG, &text])),
            Some(Some(absolute))
        );
        assert_eq!(requested(args(&["installer", "--client", &text])), None);
        // Asked for, but unusable: no fallback to the wizard.
        assert_eq!(requested(args(&["installer", FLAG])), Some(None));
        assert_eq!(requested(args(&["installer", FLAG, "rel.lzp"])), Some(None));
    }
}

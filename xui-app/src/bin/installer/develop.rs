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

use xui_app::installer::{clean, elide, Package};
use xui_app::launch;
use xui_app::platform::{argv, pkg};
use xui_core::app::{App, Ui};
use xui_core::arrange::{button, column, label, row, Entry, IntoEntry, LayoutExt};
use xui_core::backend::Result;
use xui_core::layout::Align;
use xui_core::Key;

use crate::consent::{items_list, permission_items};
use crate::view::MARGIN;

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
    fn build(ui: &mut Ui<DevMsg>, path: &Path, package: &Package) -> Result<Consent> {
        let name = elide(&package.name, 60);
        let lines = [
            format!("Run {name} from your development environment?"),
            format!(
                "{} {} by {} (unverified) will run with these permissions until you log out.",
                elide(&package.system_name, 60),
                elide(&package.version, 20),
                elide(&package.author, 40)
            ),
            "Allow only if you trust the project you are running.".to_owned(),
        ];
        let mut content: Vec<Entry<DevMsg>> = lines
            .iter()
            .map(|line| label(elide(line, 140)).into_entry())
            .collect();
        let items = permission_items(package);
        content.push(items_list(items).fill(1));
        content.push(
            row()
                .gap(12)
                .justify(Align::End)
                .children((
                    button("Allow").on_click(DevMsg::Allow).width(100),
                    // Cancel has the focus: Enter alone never consents.
                    button("Cancel")
                        .on_click(DevMsg::Deny)
                        .then_with(|cancel, ui| {
                            ui.focus(cancel.id());
                            Ok(cancel)
                        })
                        .width(100),
                ))
                .into_entry(),
        );
        ui.root(column().padding(MARGIN).gap(6).children(content))?;
        ui.on_close(|| Some(DevMsg::Deny));
        ui.on_key(|key, _| (key == Key::ESCAPE).then_some(DevMsg::Deny));
        Ok(Consent {
            path: path.to_string_lossy().into_owned(),
        })
    }
}

/// Show the consent window for `package` and act on the answer.
fn ask(path: &Path, package: Package) -> ExitCode {
    let path = path.to_path_buf();
    launch::run(
        "INSTALLER",
        "Run from development",
        (560, 400),
        move |ui, backend| {
            backend.set_size_hints(420, 300, 0, 0);
            let permissions = package.permissions.len();
            backend.on_first_frame(move || println!("INSTALLER:DEVELOP:ASK perms={permissions}"));
            Consent::build(ui, &path, &package)
                .inspect_err(|error| println!("INSTALLER:BUILD:FAIL:{error}"))
        },
    )
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

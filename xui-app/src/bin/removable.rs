//! `removable` (`os.lazy.removable`): the removable drives in the taskbar
//! tray (docs/usb-hotplug-plan.md G). A resident applet with no window,
//! shipped with the USB stack (`LAZYOS_USB=1`) and opened with every session
//! like the Volume and Network Status applets: a USB stick icon whose
//! tooltip and `Attention` flip say what happened ("MY_STICK mounted at
//! /media/MY_STICK", "can be removed", "was removed without ejecting"), a
//! badge with the number of mounted drives, and a menu with Open (Files at
//! the mount point) and Eject for each.
//!
//! The state is `mountd`'s list (`os.lazy.mount.v1` `Volumes`), looked at
//! once a second; the model that turns it into a notice and a menu is
//! `xui_app::removable` (host tested). An eject that a program holds up is
//! refused by `mountd` with who holds it, and the menu shows that line.
//!
//! Serial evidence: `REMOVABLE:UP:PASS` once it started,
//! `REMOVABLE:STATE <tooltip>` on every change of the tooltip,
//! `REMOVABLE:OPEN <path>`, `REMOVABLE:EJECT <disk> <state>`,
//! `REMOVABLE:QUIT:PASS`.

use std::time::{Duration, Instant};

use trayclient::{item, wire, Event};
use xui_app::net::mounts;
use xui_app::removable::{self, Action, Drive, Model, Phase};
use xui_app::resident::{Resident, Wake};
use xui_app::trace::{self, Channel};

/// Between looks at `mountd`'s list.
const LOOK: Duration = Duration::from_secs(1);

/// `mountd`'s volumes as the model's drives, or `None` when it did not
/// answer (not running, restarting, busy): the last list stands. An empty
/// answer would read as every drive pulled.
fn drives() -> Option<Vec<Drive>> {
    let list = mounts::volumes().ok()?;
    Some(
        list.into_iter()
            .map(|v| Drive {
                phase: Phase::of(&v.state),
                disk: v.disk,
                name: v.name,
                path: v.path,
                detail: v.detail,
            })
            .collect(),
    )
}

/// A failed call in words for the tooltip (the shared text speaks of the
/// network stack).
fn describe(error: mounts::Error) -> String {
    match error {
        mounts::Error::NotRunning => String::from("The mount service is not running"),
        other => other.describe(),
    }
}

/// The tray picture: a USB stick mask the shell tints.
fn icon() -> wire::Icon {
    wire::Icon {
        mask: Some(wire::Image {
            width: removable::MASK_SIDE,
            height: removable::MASK_SIDE,
            data: removable::mask(),
        }),
        ..wire::Icon::default()
    }
}

/// Eject `disk` and fold the answer into the model.
fn eject(model: &mut Model, disk: &str) {
    let title = model
        .drives()
        .iter()
        .find(|d| d.disk == disk)
        .map_or_else(|| disk.to_owned(), |d| d.title().to_owned());
    match mounts::eject(disk, false) {
        Ok((state, detail)) => {
            trace::line!(Channel::Removable, "EJECT {disk} {state}");
            if state == "busy" {
                model.refused(&title, &detail);
            }
        }
        Err(error) => {
            let why = describe(error);
            trace::line!(Channel::Removable, "EJECT {disk} failed: {why}");
            model.refused(&title, &why);
        }
    }
}

/// Do what a click asked for.
fn act(model: &mut Model, action: Action) {
    match action {
        Action::Open(path) => {
            let result = mounts::open_in_files(&path);
            trace::line!(
                Channel::Removable,
                "OPEN {path} {}",
                if result.is_ok() { "ok" } else { "failed" }
            );
        }
        Action::Eject(disk) => eject(model, &disk),
        Action::EjectAll(disks) => {
            for disk in disks {
                eject(model, &disk);
            }
        }
    }
}

fn main() {
    let res = Resident::windowless("REMOVABLE");
    let mut model = Model::new();
    // A first list that never comes is an empty one: nothing is plugged in
    // that this applet could know of.
    model.poll(drives().unwrap_or_default());
    let mut first = item(icon(), &model.tooltip());
    first.status = model.status();
    first.menu = model.menu();
    // Before the shell serves the tray the item is set when it announces
    // itself (`xui_app::tray`), so a refusal here is not fatal.
    if let Err(code) = res.tray.borrow_mut().set(first) {
        trace::line!(Channel::Removable, "TRAY:LATER err={}", -code);
    }
    trace::pass!(Channel::Removable, "UP");
    trace::line!(Channel::Removable, "STATE {}", model.tooltip());
    let mut shown = model.shown();
    let mut looked = Instant::now();
    loop {
        for wake in res.idle(500) {
            match wake {
                Wake::Quit(_) => {
                    let _ = res.tray.borrow_mut().clear();
                    trace::pass!(Channel::Removable, "QUIT");
                    std::process::exit(0);
                }
                Wake::Tray(Event::Activate { .. }) => {
                    if let Some(action) = model.default_action() {
                        act(&mut model, action);
                    }
                }
                Wake::Tray(Event::MenuItem { id, .. }) => {
                    if let Some(action) = model.action(id) {
                        act(&mut model, action);
                    }
                }
                _ => {}
            }
        }
        if looked.elapsed() >= LOOK {
            looked = Instant::now();
            if let Some(list) = drives() {
                model.poll(list);
            }
        }
        let now = model.shown();
        if now != shown {
            if now.0 != shown.0 {
                trace::line!(Channel::Removable, "STATE {}", now.0);
            }
            let patch = wire::UpdateArgs {
                tooltip: Some(now.0.clone()),
                status: Some(now.1),
                badge: Some(now.2.clone()),
                menu: Some(wire::Menu { rows: model.menu() }),
                ..wire::UpdateArgs::default()
            };
            let _ = res.tray.borrow_mut().update(patch);
            shown = now;
        }
    }
}

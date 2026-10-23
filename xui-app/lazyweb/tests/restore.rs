//! Bringing the last page back after a crash, and keeping it for the next time: the window driven
//! offscreen against a recording engine with a profile folder. Snapshots go to
//! `xui-app/target/snapshots/lazyweb-restore*.png`.

mod support;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use support::*;
use xui_webkit::engine::Call;
use xui_lazyweb::app::{Host, Msg};
use xui_lazyweb::prefs::{Prefs, Restore};
use xui_lazyweb::session::{self, Session};

/// The names the window told the probe, in order.
type Seen = Rc<RefCell<Vec<String>>>;

fn left_by_a_crash(home: &Path) {
    fs::write(home.join(session::RUNNING), "x").unwrap();
    session::save(
        home,
        &Session::single("https://crashed.example/page", "Before"),
    )
    .unwrap();
}

fn host_for(seen: &Seen, restore: Restore) -> impl FnOnce(Host, &Path) -> Host + 'static {
    let seen = Rc::clone(seen);
    move |host, home| Host {
        profile: Some(home.to_path_buf()),
        prefs: Prefs {
            restore,
            ..Prefs::default()
        },
        probe: Box::new(move |_, name, _| seen.borrow_mut().push(name.to_owned())),
        ..host
    }
}

fn loads(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

/// The folder `support::scratch` made for test `name`.
fn home(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lazyweb-win-{}-{name}", std::process::id()))
}

fn shown(seen: &Seen) -> bool {
    seen.borrow().iter().any(|name| name == "restore_bar")
}

#[test]
fn a_crash_offers_the_last_page_and_yes_opens_it() {
    let seen = Seen::default();
    let probe = Rc::clone(&seen);
    let (image, calls, _) = drive_with(
        "restore-yes",
        None,
        left_by_a_crash,
        host_for(&seen, Restore::Ask),
        move |stage, _| {
            assert!(shown(&probe), "the bar is up: {:?}", probe.borrow());
            stage.emit(Msg::Restore(true));
        },
    );
    let loads = loads(&calls);
    assert_eq!(loads.len(), 2, "{loads:?}");
    assert!(loads[0].starts_with("data:"), "the start page first");
    assert_eq!(loads[1], "https://crashed.example/page");
    save(&image, "lazyweb-restore-yes.png");
}

#[test]
fn the_bar_is_in_the_picture_until_it_is_answered() {
    let (image, calls, _) = drive_with(
        "restore-bar",
        None,
        left_by_a_crash,
        host_for(&Seen::default(), Restore::Ask),
        |_, _| {},
    );
    save(&image, "lazyweb-restore.png");
    assert_eq!(loads(&calls).len(), 1, "nothing is restored unasked");
}

#[test]
fn dismiss_leaves_the_start_page_and_always_restores_without_asking() {
    let (_, calls, _) = drive_with(
        "restore-no",
        None,
        left_by_a_crash,
        host_for(&Seen::default(), Restore::Ask),
        |stage, _| stage.emit(Msg::Restore(false)),
    );
    assert_eq!(loads(&calls).len(), 1, "{calls:?}");

    let seen = Seen::default();
    let (_, calls, _) = drive_with(
        "restore-auto",
        None,
        left_by_a_crash,
        host_for(&seen, Restore::Always),
        |_, _| {},
    );
    assert_eq!(loads(&calls), ["https://crashed.example/page"]);
    assert!(!shown(&seen));

    // An address the person asked for is not replaced behind their back: the bar asks instead.
    let seen = Seen::default();
    let (_, calls, _) = drive_with(
        "restore-explicit",
        Some("https://asked.example/"),
        left_by_a_crash,
        host_for(&seen, Restore::Always),
        |_, _| {},
    );
    assert_eq!(loads(&calls), ["https://asked.example/"]);
    assert!(shown(&seen));
}

#[test]
fn a_clean_start_offers_nothing_and_keeps_the_pages_it_shows() {
    let seen = Seen::default();
    let dir = home("restore-keep");
    let (_, _, _) = drive_with(
        "restore-keep",
        Some("about:blank"),
        |_| {},
        host_for(&seen, Restore::Ask),
        |stage, rig| {
            rig.state(stage, loaded("https://one.example/", "One", 1));
            // Inside the second: written later, not now.
            rig.state(stage, loaded("https://two.example/", "Two", 2));
            // The browser's own pages and files are never kept.
            rig.state(stage, loaded("data:text/html,<p>history", "History", 3));
        },
    );
    assert!(!shown(&seen));
    assert!(dir.join(session::RUNNING).is_file(), "the run is marked");
    // Written on the disk thread, as the window's quit waits for it.
    assert!(xui_lazyweb::disk::global().flush(std::time::Duration::from_secs(5)));
    let kept = session::load(&dir).expect("the session was written");
    assert_eq!(kept.page().unwrap().url, "https://one.example/");
    assert_eq!(kept.page().unwrap().title, "One");
    // A clean quit removes the marker.
    session::end(&dir);
    assert!(!dir.join(session::RUNNING).exists());
}

#[test]
fn page_data_in_memory_keeps_no_session_and_offers_none() {
    let seen = Seen::default();
    let dir = home("restore-memory");
    let (_, calls, _) = drive_with(
        "restore-memory",
        None,
        |home| {
            left_by_a_crash(home);
            xui_lazyweb::memory::set(home, true).unwrap();
        },
        host_for(&seen, Restore::Always),
        |stage, rig| rig.state(stage, loaded("https://private.example/", "Private", 1)),
    );
    assert_eq!(loads(&calls).len(), 1, "no restore, even for `always`");
    assert!(!shown(&seen));
    assert!(xui_lazyweb::disk::global().flush(std::time::Duration::from_secs(5)));
    let kept = session::load(&dir).unwrap();
    assert_eq!(
        kept.page().unwrap().url,
        "https://crashed.example/page",
        "the old file is left as it was and the new page is not written"
    );
}

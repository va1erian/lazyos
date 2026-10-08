//! The Settings window built offscreen: every section shows on its own, in
//! both modes, and a snapshot of each (`target/snapshots/settings-*.png`) is
//! left for a human to look at.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use uitheme::Mode;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::{Dip, Image};
use xui_settings::accounts_page::AccountsMsg;
use xui_settings::app::WINDOW;
use xui_settings::keyboard_page::KeyboardMsg;
use xui_settings::menu_page::MenuMsg;
use xui_settings::store::{AppChoice, ConfigStore, Value};
use xui_settings::{keyboard, theme_ops, Accounts, MemAccounts, MemStore, MemSystem, Msg};
use xui_settings::{Section, SettingsApp};

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Run `test` on its own thread with the fonts, failing if it hangs.
fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        // A worker still running past the deadline has hung: joining it would
        // hang the run too, so fail now.
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

/// A store in `mode` with a few registry apps for the Menu and Hidden pages.
fn store(mode: Mode) -> MemStore {
    let store = MemStore::new();
    theme_ops::set_mode(&store, mode).unwrap();
    *store.uid.borrow_mut() = Some(1000);
    *store.apps.borrow_mut() = ["Files", "Paint", "Terminal"]
        .iter()
        .map(|name| AppChoice {
            id: format!("os.lazy.{}", name.to_lowercase()),
            name: (*name).to_owned(),
            desktop: true,
        })
        .collect();
    store
}

/// The window in `mode` after a switch to `section`.
fn render(mode: Mode, section: Section) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
        move |ui| {
            SettingsApp::build(
                ui,
                Rc::new(store(mode)),
                Rc::new(MemSystem::default()),
                Rc::new(xui_settings::MemAccounts::default()),
            )
        },
        move |stage| stage.emit(Msg::Section(section.index())),
    )
    .expect("the headless render")
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn every_section_renders_in_both_modes() {
    watchdog(|| {
        for (mode, tag) in [(Mode::Light, "light"), (Mode::Dark, "dark")] {
            for section in Section::ALL {
                let image = render(mode, section);
                let name = section.label().to_lowercase().replace([' ', '&'], "");
                save(&image, &format!("settings-{name}-{tag}.png"));
            }
        }
    });
}

/// The window over shared doubles, after `messages`: the test reads the
/// doubles afterwards, as a session reads the services.
fn drive(store: Rc<MemStore>, accounts: Rc<MemAccounts>, messages: Vec<Msg>) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
        move |ui| SettingsApp::build(ui, store, Rc::new(MemSystem::default()), accounts),
        move |stage| {
            for msg in messages {
                stage.emit(msg);
            }
        },
    )
    .expect("the headless render")
}

#[test]
fn the_accounts_page_keeps_you_and_the_last_administrator() {
    watchdog(|| {
        let accounts = Rc::new(MemAccounts {
            me: Some(String::from("admin")),
            ..MemAccounts::default()
        });
        accounts.create("bob", "bob-pass", false).unwrap();
        let on = Msg::Accounts;
        // Rows: admin (you, the only administrator), user, bob.
        let image = drive(
            Rc::new(store(Mode::Dark)),
            accounts.clone(),
            vec![
                Msg::Section(Section::Accounts.index()),
                on(AccountsMsg::Select(0)),
                on(AccountsMsg::Remove),
                on(AccountsMsg::Admin(false)),
                on(AccountsMsg::Select(2)),
                on(AccountsMsg::Remove),
            ],
        );
        save(&image, "settings-accounts-guards.png");
        let left: Vec<(String, bool)> = accounts
            .list()
            .unwrap()
            .into_iter()
            .map(|a| (a.name, a.admin))
            .collect();
        assert_eq!(
            left,
            [("admin".to_string(), true), ("user".to_string(), false)],
            "a guard let a change through, or bob stayed"
        );
    });
}

#[test]
fn a_keyboard_layout_is_the_users_own_and_only_the_default_asks() {
    watchdog(|| {
        let kb = Msg::Keyboard;
        let page = Msg::Section(Section::Keyboard.index());
        let mem = Rc::new(store(Mode::Dark));
        let people = || Rc::new(MemAccounts::default());
        let own = "user/1000/input/layout";
        // Moving through the list writes nothing.
        let browse = vec![
            page.clone(),
            kb(KeyboardMsg::Select(1)),
            kb(KeyboardMsg::Select(0)),
        ];
        drive(mem.clone(), people(), browse);
        assert!(mem.get(own).is_none() && mem.get(keyboard::KEY_LAYOUT).is_none());
        // Apply: the account's own layout, the machine's untouched.
        let apply = vec![
            page.clone(),
            kb(KeyboardMsg::Select(1)),
            kb(KeyboardMsg::Apply),
        ];
        drive(mem.clone(), people(), apply);
        assert_eq!(mem.get(own), Some(Value::Str("fr".into())));
        assert_eq!(mem.get(keyboard::KEY_LAYOUT), None);
        // A cancelled prompt leaves the machine layout, and the account's.
        *mem.fail_writes.borrow_mut() = Some("cancelled".into());
        let refused = vec![
            page.clone(),
            kb(KeyboardMsg::Select(1)),
            kb(KeyboardMsg::MakeDefault),
        ];
        save(
            &drive(mem.clone(), people(), refused),
            "settings-keyboard-refused.png",
        );
        assert_eq!(mem.get(keyboard::KEY_LAYOUT), None);
        assert_eq!(mem.get(own), Some(Value::Str("fr".into())));
        // An approved one sets the default and the account follows it.
        *mem.fail_writes.borrow_mut() = None;
        let default = vec![page, kb(KeyboardMsg::Select(1)), kb(KeyboardMsg::MakeDefault)];
        save(
            &drive(mem.clone(), people(), default),
            "settings-keyboard-default.png",
        );
        assert_eq!(mem.get(keyboard::KEY_LAYOUT), Some(Value::Str("fr".into())));
        assert_eq!(mem.get(own), None);
    });
}

#[test]
fn menu_edits_are_saved_in_one_write() {
    watchdog(|| {
        let menu = Msg::Menu;
        let mem = Rc::new(store(Mode::Dark));
        let edits = vec![
            Msg::Section(Section::Menu.index()),
            menu(MenuMsg::Add),
            menu(MenuMsg::Add),
            menu(MenuMsg::Up),
        ];
        drive(mem.clone(), Rc::new(MemAccounts::default()), edits.clone());
        assert_eq!(
            mem.get(deskmenu::KEY),
            None,
            "an edit was written before Save"
        );
        let mut save_them = edits;
        save_them.push(menu(MenuMsg::Save));
        let image = drive(mem.clone(), Rc::new(MemAccounts::default()), save_them);
        save(&image, "settings-menu-saved.png");
        let saved = deskmenu::from_value(mem.get(deskmenu::KEY).as_ref(), &|_| true);
        let ids: Vec<&str> = saved.iter().map(|e| e.app.as_str()).collect();
        assert_eq!(ids, ["os.lazy.paint", "os.lazy.files"]);
    });
}

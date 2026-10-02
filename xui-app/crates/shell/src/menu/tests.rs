use super::*;

#[test]
fn hidden_apps_leave_both_row_groups() {
    use confd::Value;
    use deskmenu::hidden::Hidden;
    let t = Value::Bool(true);
    let f = Value::Bool(false);
    let hidden = Hidden::from_pairs(
        1000,
        [
            ("user/1000/menu/hidden/os.lazy.paint", &t),
            ("user/1000/menu/hidden/org.lazy.snake", &t),
            ("sys/menu/hidden/os.lazy.files", &t),
            ("sys/menu/hidden/os.lazy.editor", &t),
            ("user/1000/menu/hidden/os.lazy.editor", &f),
        ],
    );
    let installed = visible(
        vec![
            entry("org.lazy.snake", "Snake"),
            entry("org.lazy.dots", "Dots"),
        ],
        &hidden,
    );
    let configured = visible(deskmenu::defaults(), &hidden);
    let menu = Menu::build(&installed, &configured, Shipped::Unknown, H);
    let ids = ids(&menu);
    assert_eq!(ids[0], "org.lazy.dots");
    for gone in ["os.lazy.paint", "os.lazy.files", "org.lazy.snake"] {
        assert!(!ids.contains(&gone), "{gone} is hidden");
    }
    assert!(ids.contains(&"os.lazy.editor"), "the user un-hid it");
    assert_eq!(configured.len(), deskmenu::defaults().len() - 2);
}

#[test]
fn nothing_hidden_keeps_every_row() {
    let hidden = deskmenu::hidden::Hidden::default();
    assert_eq!(visible(deskmenu::defaults(), &hidden), deskmenu::defaults());
}

fn entry(app: &str, label: &str) -> Entry {
    Entry::new(app, label).unwrap()
}

fn ids(menu: &Menu) -> Vec<&str> {
    menu.rows().iter().map(|row| row.app.as_str()).collect()
}

const H: i32 = 768;

#[test]
fn installed_apps_come_first_then_the_configured_entries() {
    let installed = vec![entry("snake", "Snake")];
    let configured = deskmenu::defaults();
    let shipped: Vec<String> = ["terminal", "files", "snake"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let menu = Menu::build(&installed, &configured, Shipped::Known(&shipped), H);
    assert_eq!(ids(&menu)[0], "snake");
    assert_eq!(ids(&menu)[1], "terminal");
    assert_eq!(menu.rows().len(), 16, "1 installed, 13 configured, 2 power");
    assert!(menu.rows()[0].enabled);
    assert!(menu.rows()[1].enabled, "terminal is shipped");
    let docs = menu.find("os.lazy.docs").unwrap();
    assert!(!menu.rows()[docs].enabled, "unshipped rows stay, disabled");
}

#[test]
fn unknown_shipping_enables_every_row() {
    let menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Unknown, H);
    assert!(menu.rows().iter().all(|row| row.enabled));
}

#[test]
fn configured_rows_keep_the_contract_centres() {
    // Default m = 13: Terminal (j = 0) at y = H-384, Devices (j = 12) at
    // y = H-96, then the power rows at H-72 and H-48, x = 134; with or
    // without installed rows above them.
    for installed in [
        vec![],
        vec![entry("snake", "Snake"), entry("chess", "Chess")],
    ] {
        let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, H);
        let (ox, oy) = menu.origin(H);
        assert_eq!(oy + menu.height(), H - 32, "flush with the taskbar top");
        let centre = |app: &str| {
            let rect = menu.row_rect(menu.find(app).unwrap()).unwrap();
            (ox + rect.x + rect.w / 2, oy + rect.y + rect.h / 2)
        };
        assert_eq!(centre("terminal"), (134, H - 384));
        assert_eq!(centre("installer"), (134, H - 120));
        assert_eq!(centre("devices"), (134, H - 96));
        let power = |action| {
            let rect = menu.row_rect(menu.find_action(action).unwrap()).unwrap();
            (ox + rect.x + rect.w / 2, oy + rect.y + rect.h / 2)
        };
        assert_eq!(power(Action::Ask(Power::Reboot)), (134, H - 72));
        assert_eq!(power(Action::Ask(Power::PowerOff)), (134, H - 48));
    }
}

#[test]
fn a_configured_app_is_not_listed_twice() {
    let installed = vec![entry("terminal", "Terminal (pkg)"), entry("snake", "Snake")];
    let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, H);
    assert_eq!(ids(&menu).iter().filter(|id| **id == "terminal").count(), 1);
    assert_eq!(menu.rows()[0].app, "snake");
}

#[test]
fn rows_that_do_not_fit_drop_installed_apps_first() {
    let installed: Vec<Entry> = (0..20).map(|i| entry(&format!("app{i}"), "x")).collect();
    let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, 400);
    // (400 - 32 - 8) / 24 = 15 rows: 13 configured + 2 power, no installed.
    assert_eq!(menu.rows().len(), 15);
    assert_eq!(menu.rows()[0].app, "terminal");
    assert!(menu.origin(400).1 >= 0);
    let roomier = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, 448);
    assert_eq!(roomier.rows()[2].app, "terminal", "17 rows: 2 installed");
    let roomy = Menu::build(&installed, &[], Shipped::Unknown, 2000);
    assert_eq!(roomy.rows().len(), MAX_INSTALLED + POWER_ROWS);
}

#[test]
fn row_hit_testing_skips_the_banner_and_padding() {
    let menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Unknown, H);
    assert_eq!(menu.row_at(100, PAD), Some(0));
    assert_eq!(menu.row_at(100, PAD + ROW_H), Some(1));
    assert_eq!(menu.row_at(10, PAD + 5), None, "the banner");
    assert_eq!(menu.row_at(100, 1), None, "top padding");
    assert_eq!(menu.row_at(100, menu.height() - 2), None, "bottom padding");
    assert_eq!(menu.row_at(WIDTH, PAD + 5), None);
    assert_eq!(menu.row_rect(14).map(|r| r.y), Some(PAD + 14 * ROW_H));
    assert_eq!(menu.row_rect(15), None);
}

#[test]
fn installed_rows_come_from_the_registry_flag() {
    let apps = [
        ("terminal", "Terminal", false),
        ("snake", "  Snake\u{7} ", true),
        ("chess", "", true),
        ("Bad Id", "x", true),
        // A package id is its reverse-DNS system name (pkg_install.json).
        ("org.lazy.counter", "Packaged Counter", true),
    ];
    let rows = installed_entries(apps);
    let packaged = Entry {
        app: String::from("org.lazy.counter"),
        label: String::from("Packaged Counter"),
    };
    assert_eq!(
        rows,
        vec![entry("snake", "Snake"), entry("chess", "chess"), packaged]
    );
}

#[test]
fn the_power_rows_come_last_whatever_is_configured() {
    for configured in [vec![], deskmenu::defaults()] {
        let menu = Menu::build(&[], &configured, Shipped::Unknown, H);
        let labels: Vec<&str> = menu.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels[labels.len() - 2..], ["Restart...", "Shut down..."]);
        assert!(menu.rows()[labels.len() - 2..].iter().all(|r| r.enabled));
        assert_eq!(menu.find(""), None, "a power row is never an app");
    }
}

#[test]
fn a_power_row_asks_for_confirmation_in_place() {
    for (power, now) in [
        (Power::Reboot, "Restart now"),
        (Power::PowerOff, "Shut down now"),
    ] {
        let mut menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Unknown, H);
        let (count, height) = (menu.rows().len(), menu.height());
        let ask = menu.find_action(Action::Ask(power)).unwrap();
        assert!(!menu.confirming());
        assert_eq!(menu.choose(ask, false), Choice::Confirming(power));
        assert!(menu.confirming());
        assert_eq!((menu.rows().len(), menu.height()), (count, height));
        let labels: Vec<&str> = menu.rows()[count - 2..]
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, [now, "Cancel"]);
        assert_eq!(menu.rows()[0].app, "terminal", "the app rows stay");
        // The second press of a double click never confirms.
        assert_eq!(menu.choose(count - 2, true), Choice::Nothing);
        assert_eq!(menu.choose(count - 2, false), Choice::Request(power));
    }
}

#[test]
fn cancel_restores_the_power_rows_and_closes() {
    let mut menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Unknown, H);
    let fresh = menu.clone();
    let off = menu.find_action(Action::Ask(Power::PowerOff)).unwrap();
    assert_eq!(menu.choose(off, false), Choice::Confirming(Power::PowerOff));
    let cancel = menu.find_action(Action::Cancel).unwrap();
    assert_eq!(menu.choose(cancel, false), Choice::Close);
    assert_eq!(menu, fresh);
}

#[test]
fn choosing_launches_enabled_apps_only() {
    let shipped = vec![String::from("terminal")];
    let mut menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Known(&shipped), H);
    let terminal = menu.find("terminal").unwrap();
    assert_eq!(
        menu.choose(terminal, true),
        Choice::Launch(String::from("terminal"))
    );
    let docs = menu.find("os.lazy.docs").unwrap();
    assert_eq!(menu.choose(docs, false), Choice::Nothing);
    assert_eq!(menu.choose(99, false), Choice::Nothing);
}

#[test]
fn a_menu_saved_with_short_ids_still_matches_the_core_apps() {
    // `sys/ui/menu` from before F5 names `editor`; `init` lists
    // `os.lazy.editor`, which launches for the short id too (issue #509).
    let configured = vec![entry("editor", "Editor")];
    let installed = vec![entry("os.lazy.editor", "Editor")];
    let shipped = vec![String::from("os.lazy.editor")];
    let menu = Menu::build(&installed, &configured, Shipped::Known(&shipped), H);
    assert_eq!(ids(&menu)[0], "editor", "no second Editor row");
    assert!(menu.rows()[0].enabled, "the short id is shipped");
}

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
    let hides = |id: &str| hidden.hides(id);
    let installed: Vec<InstalledApp> =
        [app("org.lazy.snake", "Snake"), app("org.lazy.dots", "Dots")]
            .into_iter()
            .filter(|app| !hides(&app.entry.app))
            .collect();
    let configured = visible(deskmenu::defaults(), hides);
    let menu = Menu::build(&installed, &configured, Shipped::Unknown, H);
    let ids = ids(&menu);
    assert_eq!(ids[1], "org.lazy.dots", "under its category header");
    for gone in ["os.lazy.paint", "os.lazy.files", "org.lazy.snake"] {
        assert!(!ids.contains(&gone), "{gone} is hidden");
    }
    assert!(ids.contains(&"os.lazy.editor"), "the user un-hid it");
    assert_eq!(configured.len(), deskmenu::defaults().len() - 2);
}

#[test]
fn nothing_hidden_keeps_every_row() {
    let hidden = deskmenu::hidden::Hidden::default();
    assert_eq!(
        visible(deskmenu::defaults(), |id| hidden.hides(id)),
        deskmenu::defaults()
    );
}

fn entry(app: &str, label: &str) -> Entry {
    Entry::new(app, label).unwrap()
}

/// An installed app in the default category.
fn app(id: &str, label: &str) -> InstalledApp {
    in_category(id, label, "accessories")
}

fn in_category(id: &str, label: &str, category: &str) -> InstalledApp {
    InstalledApp {
        entry: entry(id, label),
        category: String::from(category),
    }
}

fn ids(menu: &Menu) -> Vec<&str> {
    menu.rows().iter().map(|row| row.app.as_str()).collect()
}

const H: i32 = 768;

#[test]
fn installed_apps_come_first_then_the_configured_entries() {
    let installed = vec![app("snake", "Snake")];
    let configured = deskmenu::defaults();
    let shipped: Vec<String> = ["terminal", "files", "snake"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let menu = Menu::build(&installed, &configured, Shipped::Known(&shipped), H);
    assert_eq!(menu.rows()[0].label, "Accessories");
    assert_eq!(menu.rows()[0].action, Action::Header);
    assert!(!menu.rows()[0].enabled, "a header is never chosen");
    assert_eq!(ids(&menu)[1], "snake");
    assert_eq!(ids(&menu)[2], "terminal");
    assert_eq!(
        menu.rows().len(),
        17,
        "header, 1 installed, 13 configured, 2 power"
    );
    assert!(menu.rows()[1].enabled);
    assert!(menu.rows()[2].enabled, "terminal is shipped");
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
        vec![
            app("snake", "Snake"),
            in_category("chess", "Chess", "office"),
        ],
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
    let installed = vec![app("terminal", "Terminal (pkg)"), app("snake", "Snake")];
    let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, H);
    assert_eq!(ids(&menu).iter().filter(|id| **id == "terminal").count(), 1);
    assert_eq!(menu.rows()[1].app, "snake");
}

#[test]
fn rows_that_do_not_fit_drop_installed_apps_first() {
    let installed: Vec<InstalledApp> = (0..20)
        .map(|i| app(&format!("app{i:02}"), &format!("x{i:02}")))
        .collect();
    let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, 400);
    // (400 - 32 - 8) / 24 = 15 rows: 13 configured + 2 power, no installed.
    assert_eq!(menu.rows().len(), 15);
    assert_eq!(menu.rows()[0].app, "terminal");
    assert!(menu.origin(400).1 >= 0);
    let roomier = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, 448);
    assert_eq!(
        roomier.rows()[2].app,
        "terminal",
        "17 rows: 2 of the section"
    );
    // One category lists at most MAX_PER_CATEGORY apps, plus its header.
    let roomy = Menu::build(&installed, &[], Shipped::Unknown, 2000);
    assert_eq!(roomy.rows().len(), 1 + MAX_PER_CATEGORY + POWER_ROWS);
    assert_eq!(roomy.scroll(), None, "everything fits");
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
    let rows = installed_entries(apps.iter().map(|(id, name, installed)| Listed {
        id,
        name,
        installed: *installed,
        category: if *installed { "graphics" } else { "" },
        hidden: *id == "chess",
    }));
    let packaged = InstalledApp {
        entry: Entry {
            app: String::from("org.lazy.counter"),
            label: String::from("Packaged Counter"),
        },
        category: String::from("graphics"),
    };
    let snake = in_category("snake", "Snake", "graphics");
    assert_eq!(rows, vec![snake, packaged], "chess is hidden");
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
    let installed = vec![app("os.lazy.editor", "Editor")];
    let shipped = vec![String::from("os.lazy.editor")];
    let menu = Menu::build(&installed, &configured, Shipped::Known(&shipped), H);
    assert_eq!(ids(&menu)[0], "editor", "no second Editor row");
    assert!(menu.rows()[0].enabled, "the short id is shipped");
}

#[test]
fn installed_apps_group_by_category_in_menu_order() {
    let installed = vec![
        in_category("org.x.zeta", "Zeta", "system"),
        in_category("org.x.paint", "paint", "graphics"),
        in_category("org.x.alpha", "Alpha", "graphics"),
        in_category("org.x.odd", "Odd", "no-such-category"),
    ];
    let menu = Menu::build(&installed, &[], Shipped::Unknown, H);
    let labels: Vec<&str> = menu.rows().iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels[..7],
        [
            "Accessories",
            "Odd",
            "Graphics",
            "Alpha",
            "paint",
            "System",
            "Zeta"
        ],
        "an unknown category files under accessories; labels sort without case"
    );
    let headers = menu
        .rows()
        .iter()
        .filter(|r| r.action == Action::Header)
        .count();
    assert_eq!(headers, 3);
    assert_eq!(menu.find(""), None, "a header is never an app");
}

#[test]
fn a_tall_section_scrolls_and_the_configured_rows_stay_put() {
    // Two full categories: 2 headers + 32 apps = 34 section rows.
    let mut installed: Vec<InstalledApp> = (0..20)
        .map(|i| in_category(&format!("org.a.app{i:02}"), &format!("A{i:02}"), "office"))
        .collect();
    installed.extend((0..20).map(|i| {
        in_category(
            &format!("org.b.app{i:02}"),
            &format!("B{i:02}"),
            "utilities",
        )
    }));
    let mut menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, H);
    // (768 - 32 - 8) / 24 = 30 rows: 13 configured + 2 power leave 15.
    let scroll = menu.scroll().expect("34 rows do not fit in 15");
    assert_eq!((scroll.first, scroll.shown, scroll.total), (0, 15, 34));
    assert_eq!(menu.rows()[0].label, "Office");
    let terminal = menu.find("terminal").unwrap();
    assert!(!menu.scroll_by(-3), "already at the top");
    assert!(menu.scroll_by(3));
    assert_eq!(menu.rows()[0].label, "A02");
    assert_eq!(
        menu.find("terminal"),
        Some(terminal),
        "configured rows do not move"
    );
    assert!(menu.scroll_by(1000));
    let end = menu.scroll().unwrap();
    assert_eq!(end.first, 34 - 15);
    assert_eq!(
        menu.rows()[14].label,
        "B15",
        "the last listed app of the last category"
    );
    assert!(!menu.scroll_by(1));
    assert!(menu.scroll_by(-1000));
    assert_eq!(menu.rows()[0].label, "Office");
}

#[test]
fn scrolling_keeps_a_pending_power_confirmation() {
    let installed: Vec<InstalledApp> = (0..16)
        .map(|i| app(&format!("org.a.app{i:02}"), &format!("A{i:02}")))
        .collect();
    let mut menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, H);
    let off = menu.find_action(Action::Ask(Power::PowerOff)).unwrap();
    assert_eq!(menu.choose(off, false), Choice::Confirming(Power::PowerOff));
    assert!(menu.scroll_by(1));
    assert!(menu.confirming());
}

#[test]
fn hidden_flags_from_list_apps_match_short_ids() {
    let listed = [
        Listed {
            id: "os.lazy.paint",
            name: "Paint",
            installed: true,
            category: "graphics",
            hidden: true,
        },
        Listed {
            id: "terminal",
            name: "Terminal",
            installed: false,
            category: "",
            hidden: false,
        },
    ];
    assert!(listed_hidden(&listed, "os.lazy.paint"));
    assert!(listed_hidden(&listed, "paint"), "a pre-F5 short id");
    assert!(!listed_hidden(&listed, "terminal"));
    let configured = visible(deskmenu::defaults(), |id| listed_hidden(&listed, id));
    assert!(configured.iter().all(|e| e.app != "os.lazy.paint"));
    assert_eq!(configured.len(), deskmenu::defaults().len() - 1);
}

#[test]
fn built_in_desktop_programs_are_listed_and_console_ones_are_not() {
    let listed = [
        Listed {
            id: "terminal",
            name: "Terminal",
            installed: false,
            category: "system",
            hidden: false,
        },
        Listed {
            id: "top",
            name: "System Monitor (text)",
            installed: false,
            category: "",
            hidden: false,
        },
        Listed {
            id: "os.lazy.paint",
            name: "Paint",
            installed: true,
            category: "graphics",
            hidden: false,
        },
    ];
    let rows = installed_entries(listed.iter().copied());
    let ids: Vec<&str> = rows.iter().map(|a| a.entry.app.as_str()).collect();
    assert_eq!(ids, ["terminal", "os.lazy.paint"]);
    // A menu that pins neither shows the Terminal under System.
    let menu = Menu::build(&rows, &[], Shipped::Unknown, H);
    let labels: Vec<&str> = menu.rows().iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels[..4], ["Graphics", "Paint", "System", "Terminal"]);
}

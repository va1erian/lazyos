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
    let configured = visible(pinned(), hides);
    let menu = Menu::build(&installed, &configured, Shipped::Unknown, H);
    let ids = ids(&menu);
    let listed = sub_ids(&menu, "accessories");
    assert_eq!(listed, ["org.lazy.dots"], "in its category's submenu");
    for gone in ["os.lazy.paint", "os.lazy.files", "org.lazy.snake"] {
        assert!(!ids.contains(&gone), "{gone} is hidden");
    }
    assert!(ids.contains(&"os.lazy.editor"), "the user un-hid it");
    assert_eq!(configured.len(), pinned().len() - 2);
}

#[test]
fn nothing_hidden_keeps_every_row() {
    let hidden = deskmenu::hidden::Hidden::default();
    assert_eq!(visible(pinned(), |id| hidden.hides(id)), pinned());
}

/// A pinned list (`sys/ui/menu`) of 13 apps, the root rows the menu showed
/// before nothing was pinned by default.
fn pinned() -> Vec<Entry> {
    [
        ("terminal", "Terminal"),
        ("os.lazy.sysmon", "System Monitor"),
        ("os.lazy.fabricmon", "Fabric Monitor"),
        ("os.lazy.counter", "Counter"),
        ("os.lazy.editor", "Editor"),
        ("os.lazy.paint", "Paint"),
        ("os.lazy.files", "Files"),
        ("os.lazy.settings", "Settings"),
        ("os.lazy.docs", "Docs"),
        ("os.lazy.widget", "CPU & Memory"),
        ("os.lazy.confd", "Config"),
        ("installer", "Package Installer"),
        ("devices", "Devices"),
    ]
    .iter()
    .map(|(app, label)| entry(app, label))
    .collect()
}

#[test]
fn by_default_the_root_is_the_categories_and_the_power_rows() {
    let installed = vec![
        in_category("terminal", "Terminal", "system"),
        in_category("os.lazy.settings", "Settings", "system"),
        app("os.lazy.counter", "Counter"),
    ];
    let menu = Menu::build(&installed, &deskmenu::defaults(), Shipped::Unknown, 720);
    assert_eq!(
        labels(&menu),
        ["Accessories", "System", "Restart...", "Shut down..."]
    );
    assert!(menu.rows().iter().all(|row| row.action != Action::Launch));
    assert_eq!(sub_ids(&menu, "system"), ["os.lazy.settings", "terminal"]);
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

/// The apps category `id`'s submenu lists.
fn sub_ids(menu: &Menu, id: &str) -> Vec<String> {
    let row = menu.find_category(id).expect("category row");
    let sub = menu.submenu(row, H).expect("a submenu");
    sub.rows().iter().map(|row| row.app.clone()).collect()
}

/// The rows' labels, top-down.
fn labels(menu: &Menu) -> Vec<&str> {
    menu.rows().iter().map(|row| row.label.as_str()).collect()
}

const H: i32 = 768;

#[test]
fn installed_apps_come_first_then_the_configured_entries() {
    let installed = vec![app("snake", "Snake")];
    let configured = pinned();
    let shipped: Vec<String> = ["terminal", "files", "snake"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let menu = Menu::build(&installed, &configured, Shipped::Known(&shipped), H);
    assert_eq!(menu.rows()[0].label, "Accessories");
    assert_eq!(menu.rows()[0].action, Action::Submenu(0));
    assert!(menu.rows()[0].enabled, "a category row opens its submenu");
    assert_eq!(sub_ids(&menu, "accessories"), ["snake"]);
    assert_eq!(ids(&menu)[1], "terminal");
    assert_eq!(menu.rows().len(), 16, "1 category, 13 configured, 2 power");
    assert!(menu.rows()[1].enabled, "terminal is shipped");
    let docs = menu.find("os.lazy.docs").unwrap();
    assert!(!menu.rows()[docs].enabled, "unshipped rows stay, disabled");
}

#[test]
fn unknown_shipping_enables_every_row() {
    let menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
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
        let menu = Menu::build(&installed, &pinned(), Shipped::Unknown, H);
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
fn a_pinned_app_is_also_in_its_category() {
    let installed = vec![app("terminal", "Terminal (pkg)"), app("snake", "Snake")];
    let menu = Menu::build(&installed, &pinned(), Shipped::Unknown, H);
    assert_eq!(ids(&menu).iter().filter(|id| **id == "terminal").count(), 1);
    assert_eq!(sub_ids(&menu, "accessories"), ["snake", "terminal"]);
}

/// One app in each of the eight categories.
fn one_per_category() -> Vec<InstalledApp> {
    groups::CATEGORIES
        .iter()
        .map(|(id, _)| in_category(&format!("org.x.{id}"), id, id))
        .collect()
}

#[test]
fn rows_that_do_not_fit_drop_category_rows_first() {
    let installed = one_per_category();
    let menu = Menu::build(&installed, &pinned(), Shipped::Unknown, 400);
    // (400 - 32 - 8) / 24 = 15 rows: 13 configured + 2 power, no category.
    assert_eq!(menu.rows().len(), 15);
    assert_eq!(menu.rows()[0].app, "terminal");
    assert!(menu.origin(400).1 >= 0);
    let roomier = Menu::build(&installed, &pinned(), Shipped::Unknown, 448);
    assert_eq!(
        roomier.rows()[2].app,
        "terminal",
        "17 rows: 2 of the section"
    );
    let roomy = Menu::build(&installed, &[], Shipped::Unknown, 2000);
    assert_eq!(roomy.rows().len(), 8 + POWER_ROWS, "one row per category");
    assert_eq!(roomy.scroll(), None, "everything fits");
}

#[test]
fn a_submenu_lists_at_most_max_per_category() {
    let installed: Vec<InstalledApp> = (0..20)
        .map(|i| app(&format!("app{i:02}"), &format!("x{i:02}")))
        .collect();
    let menu = Menu::build(&installed, &[], Shipped::Unknown, 2000);
    assert_eq!(menu.rows().len(), 1 + POWER_ROWS);
    let sub = menu.submenu(0, 2000).unwrap();
    assert_eq!(sub.rows().len(), MAX_PER_CATEGORY);
    assert_eq!(sub.app(0), Some("app00"));
}

#[test]
fn row_hit_testing_skips_the_banner_and_padding() {
    let menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
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
    for configured in [vec![], pinned()] {
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
        let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
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
    let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
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
    let mut menu = Menu::build(&[], &pinned(), Shipped::Known(&shipped), H);
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
    assert_eq!(ids(&menu)[1], "editor", "under the category row");
    assert!(menu.rows()[1].enabled, "the short id is shipped");
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
    assert_eq!(
        labels(&menu)[..3],
        ["Accessories", "Graphics", "System"],
        "an unknown category files under accessories"
    );
    assert_eq!(sub_ids(&menu, "accessories"), ["org.x.odd"]);
    assert_eq!(
        sub_ids(&menu, "graphics"),
        ["org.x.alpha", "org.x.paint"],
        "labels sort without case"
    );
    assert_eq!(sub_ids(&menu, "system"), ["org.x.zeta"]);
    assert_eq!(menu.find(""), None, "a category row is never an app");
}

#[test]
fn a_tall_section_scrolls_and_the_configured_rows_stay_put() {
    let installed = one_per_category();
    // (448 - 32 - 8) / 24 = 17 rows: 13 configured + 2 power leave 2.
    let mut menu = Menu::build(&installed, &pinned(), Shipped::Unknown, 448);
    let scroll = menu.scroll().expect("8 rows do not fit in 2");
    assert_eq!((scroll.first, scroll.shown, scroll.total), (0, 2, 8));
    assert_eq!(menu.rows()[0].label, "Accessories");
    let terminal = menu.find("terminal").unwrap();
    assert!(!menu.scroll_by(-3), "already at the top");
    assert!(menu.scroll_by(3));
    assert_eq!(menu.rows()[0].label, "Graphics");
    assert_eq!(menu.rows()[0].action, Action::Submenu(3));
    assert_eq!(
        menu.find("terminal"),
        Some(terminal),
        "configured rows do not move"
    );
    assert!(menu.scroll_by(1000));
    assert_eq!(menu.scroll().unwrap().first, 6);
    assert_eq!(menu.rows()[1].label, "Utilities", "the last category");
    assert!(!menu.scroll_by(1));
    assert!(menu.scroll_by(-1000));
    assert_eq!(menu.rows()[0].label, "Accessories");
}

#[test]
fn scrolling_keeps_a_pending_power_confirmation() {
    let mut menu = Menu::build(&one_per_category(), &pinned(), Shipped::Unknown, 448);
    let off = menu.find_action(Action::Ask(Power::PowerOff)).unwrap();
    assert_eq!(menu.choose(off, false), Choice::Confirming(Power::PowerOff));
    assert!(menu.scroll_by(1));
    assert!(menu.confirming());
}

#[test]
fn a_category_row_opens_its_submenu_beside_it() {
    let installed = vec![
        in_category("org.x.chess", "Chess", "games"),
        in_category("org.x.snake", "Snake", "games"),
        in_category("org.x.paint", "Paint", "graphics"),
    ];
    let mut menu = Menu::build(&installed, &pinned(), Shipped::Unknown, H);
    let games = menu.find_category("games").unwrap();
    assert_eq!(menu.choose(games, false), Choice::Submenu(games));
    let sub = menu.submenu(games, H).unwrap();
    assert_eq!(sub.id, "games");
    let (_, oy) = menu.origin(H);
    let row_top = oy + menu.row_rect(games).unwrap().y;
    let (sx, sy) = sub.origin();
    assert_eq!(sx, WIDTH - 2, "overlapping the menu's right edge");
    assert_eq!(sy + PAD, row_top, "first row level with the category row");
    assert_eq!(sub.height(), 2 * ROW_H + 2 * PAD);
    assert_eq!(sub.row_at(10, PAD + ROW_H + 3), Some(1));
    assert_eq!(sub.row_at(SUB_WIDTH, PAD + 3), None);
    assert_eq!(sub.app(1), Some("org.x.snake"));
    assert_eq!(sub.find("org.x.chess"), Some(0));
    assert!(menu.submenu(menu.find("terminal").unwrap(), H).is_none());
}

#[test]
fn a_low_submenu_slides_up_onto_the_taskbar_edge() {
    let installed: Vec<InstalledApp> = (0..10)
        .map(|i| in_category(&format!("org.x.a{i}"), &format!("A{i}"), "utilities"))
        .collect();
    let category = &groups::categories(&installed)[0];
    let sub = Submenu::new(0, category, H - 60, H);
    let (_, y) = sub.origin();
    assert_eq!(y + sub.height(), H - 32, "flush with the taskbar top");
    // A screen too short for all ten keeps what fits, from the top.
    let short = Submenu::new(0, category, 10, 200);
    assert_eq!(short.rows().len(), ((200 - 32 - 8) / ROW_H) as usize);
    assert_eq!(
        short.origin().1,
        10 - PAD,
        "it fits below the row: no slide"
    );
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
    let configured = visible(pinned(), |id| listed_hidden(&listed, id));
    assert!(configured.iter().all(|e| e.app != "os.lazy.paint"));
    assert_eq!(configured.len(), pinned().len() - 1);
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
    assert_eq!(labels(&menu)[..2], ["Graphics", "System"]);
    assert_eq!(sub_ids(&menu, "system"), ["terminal"]);
}

use super::*;

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
    assert_eq!(menu.rows().len(), 14);
    assert!(menu.rows()[0].enabled);
    assert!(menu.rows()[1].enabled, "terminal is shipped");
    let docs = menu.find("docs").unwrap();
    assert!(!menu.rows()[docs].enabled, "unshipped rows stay, disabled");
}

#[test]
fn unknown_shipping_enables_every_row() {
    let menu = Menu::build(&[], &deskmenu::defaults(), Shipped::Unknown, H);
    assert!(menu.rows().iter().all(|row| row.enabled));
}

#[test]
fn configured_rows_keep_the_contract_centres() {
    // Default m = 13: Terminal (j = 0) at y = H-336, Devices (j = 12) at
    // y = H-48, x = 134; with or without installed rows above them.
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
        assert_eq!(centre("terminal"), (134, H - 336));
        assert_eq!(centre("installer"), (134, H - 72));
        assert_eq!(centre("devices"), (134, H - 48));
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
    // (400 - 32 - 8) / 24 = 15 rows: 13 configured + 2 installed.
    assert_eq!(menu.rows().len(), 15);
    assert_eq!(menu.rows()[2].app, "terminal");
    assert!(menu.origin(400).1 >= 0);
    let roomy = Menu::build(&installed, &[], Shipped::Unknown, 2000);
    assert_eq!(roomy.rows().len(), MAX_INSTALLED);
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
    assert_eq!(menu.row_rect(13), None);
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

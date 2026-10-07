use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::item::Item;
use super::menu::{self, default_pick, Kind, Pick, TrayMenu, PAD, ROW_H, SEPARATOR_H, WIDTH};
use crate::Rect;

fn row(id: u32, parent: u32, kind: u32, label: &str) -> wire::MenuItem {
    wire::MenuItem {
        id,
        parent,
        label: label.into(),
        kind,
        enabled: true,
        ..wire::MenuItem::default()
    }
}

/// The traydemo's menu: a default row, a check, a separator and a submenu
/// of radios.
fn rows() -> Vec<super::item::MenuRow> {
    let mut show = row(1, 0, wire::MENU_KIND_NORMAL, "Show window");
    show.is_default = true;
    let mut lucide = row(5, 4, wire::MENU_KIND_RADIO, "Lucide");
    lucide.checked = true;
    let mut off = row(8, 0, wire::MENU_KIND_NORMAL, "Disabled");
    off.enabled = false;
    let item = wire::Item {
        menu: vec![
            show,
            row(2, 0, wire::MENU_KIND_CHECK, "Notifications"),
            row(3, 0, wire::MENU_KIND_SEPARATOR, ""),
            row(4, 0, wire::MENU_KIND_SUBMENU, "Icon"),
            lucide,
            row(6, 4, wire::MENU_KIND_RADIO, "Pixels"),
            off,
        ],
        ..wire::Item::default()
    };
    Item::from_wire(item).unwrap().menu
}

#[test]
fn the_top_level_ends_with_the_shells_quit_row() {
    let menu = TrayMenu::top(&rows(), "Tray Demo");
    let labels: Vec<&str> = menu.rows().iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Show window",
            "Notifications",
            "",
            "Icon",
            "Disabled",
            "",
            "Quit Tray Demo"
        ]
    );
    assert_eq!(menu.rows().last().unwrap().kind, Kind::Quit);
    assert_eq!(menu.pick(6), Some(Pick::Quit));
    // No app rows: the Quit row alone, no separator before it.
    let bare = TrayMenu::top(&[], "Volume");
    assert_eq!(bare.rows().len(), 1);
    assert_eq!(bare.height(), 2 * PAD + ROW_H);
}

#[test]
fn an_app_cannot_relabel_or_hide_quit() {
    // A row labelled like the Quit row is just an app row.
    let mut fake = rows();
    fake.push(super::item::MenuRow {
        id: 9,
        parent: 0,
        label: "Quit Tray Demo".into(),
        kind: super::item::MenuKind::Normal,
        enabled: true,
        checked: false,
        default: false,
    });
    let menu = TrayMenu::top(&fake, "Tray Demo");
    let quits = menu.rows().iter().filter(|r| r.kind == Kind::Quit).count();
    assert_eq!(quits, 1);
    assert_eq!(
        menu.rows().last().unwrap().label,
        menu::quit_label("Tray Demo")
    );
    assert_eq!(
        menu.pick(menu.rows().len() - 3),
        Some(Pick::Item {
            id: 9,
            checked: false
        })
    );
}

#[test]
fn picks_flip_checks_set_radios_and_skip_separators() {
    let menu = TrayMenu::top(&rows(), "Tray Demo");
    assert_eq!(
        menu.pick(0),
        Some(Pick::Item {
            id: 1,
            checked: false
        })
    );
    assert_eq!(
        menu.pick(1),
        Some(Pick::Item {
            id: 2,
            checked: true
        })
    );
    assert_eq!(menu.pick(2), None, "separator");
    assert_eq!(menu.pick(3), Some(Pick::Submenu { id: 4 }));
    assert_eq!(menu.pick(4), None, "disabled");
    let sub = TrayMenu::submenu(&rows(), 4);
    assert_eq!(sub.rows().len(), 2);
    assert_eq!(
        sub.pick(0),
        Some(Pick::Item {
            id: 5,
            checked: true
        })
    );
    assert_eq!(
        sub.pick(1),
        Some(Pick::Item {
            id: 6,
            checked: true
        })
    );
    assert_eq!(
        default_pick(&rows()),
        Some(Pick::Item {
            id: 1,
            checked: false
        })
    );
    assert_eq!(default_pick(&[]), None);
}

#[test]
fn geometry_stacks_rows_and_separators() {
    let menu = TrayMenu::top(&rows(), "Tray Demo");
    assert_eq!(menu.row_rect(0), Some(Rect::new(0, PAD, WIDTH, ROW_H)));
    assert_eq!(
        menu.row_rect(2),
        Some(Rect::new(0, PAD + 2 * ROW_H, WIDTH, SEPARATOR_H))
    );
    assert_eq!(
        menu.row_rect(3),
        Some(Rect::new(0, PAD + 2 * ROW_H + SEPARATOR_H, WIDTH, ROW_H))
    );
    assert_eq!(menu.height(), 2 * PAD + 5 * ROW_H + 2 * SEPARATOR_H);
    assert_eq!(menu.row_at(5, PAD + 1), Some(0));
    assert_eq!(menu.row_at(5, 0), None);
    assert_eq!(menu.row_at(WIDTH, PAD + 1), None);
}

#[test]
fn the_panel_opens_above_the_bar_and_stays_on_screen() {
    let screen = (1280, 720);
    let cell = Rect::new(1100, 692, 24, 24);
    assert_eq!(
        TrayMenu::origin(cell, 100, screen, 688),
        (1124 - WIDTH, 588)
    );
    let left = Rect::new(10, 692, 24, 24);
    assert_eq!(TrayMenu::origin(left, 100, screen, 688), (0, 588));
    assert_eq!(TrayMenu::origin(cell, 900, screen, 688), (1124 - WIDTH, 0));
}

#[test]
fn a_default_item_offers_open_and_quit() {
    let menu = TrayMenu::default_item("Volume");
    let kinds: Vec<&Kind> = menu.rows().iter().map(|row| &row.kind).collect();
    assert_eq!(kinds, [&Kind::Open, &Kind::Separator, &Kind::Quit]);
    assert_eq!(menu.pick(0), Some(Pick::Open));
    assert_eq!(menu.pick(2), Some(Pick::Quit));
    assert_eq!(menu.rows()[2].label, "Quit Volume");
}

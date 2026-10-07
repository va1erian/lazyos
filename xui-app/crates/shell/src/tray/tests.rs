use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::icon::{self, Picture};
use super::item::{self, Activation, Invalid, Item, MenuKind, Patch, Source, Status};
use super::layout::{self, Hit, CELL, CHEVRON_W, TRAY_GAP, TRAY_VISIBLE};
use super::{Change, Refused, Tray, ITEMS_MAX};
use crate::taskbar::{self, ENTRY_X};
use crate::Rect;

fn image(w: u32, h: u32) -> wire::Image {
    wire::Image {
        width: w,
        height: h,
        data: vec![0xff; (w * h * 4) as usize],
    }
}

fn lucide(name: &str) -> wire::Icon {
    wire::Icon {
        lucide: Some(name.into()),
        ..wire::Icon::default()
    }
}

fn row(id: u32, parent: u32, kind: u32) -> wire::MenuItem {
    wire::MenuItem {
        id,
        parent,
        label: format!("Row {id}"),
        kind,
        enabled: true,
        ..wire::MenuItem::default()
    }
}

fn valid() -> wire::Item {
    wire::Item {
        icon: lucide("volume-2"),
        tooltip: "Volume 40%".into(),
        status: wire::STATUS_ACTIVE,
        badge: None,
        menu: vec![row(1, 0, wire::MENU_KIND_CHECK)],
        activate: wire::ACTIVATION_EVENT,
    }
}

fn item(status: Status) -> Item {
    Item {
        status,
        ..Item::default()
    }
}

#[test]
fn a_valid_item_keeps_every_field() {
    let mut wire_item = valid();
    wire_item.badge = Some(" 3 ".into());
    wire_item.tooltip = "line\none".into();
    let item = Item::from_wire(wire_item).unwrap();
    assert_eq!(item.icon, Some(Source::Lucide("volume-2".into())));
    assert_eq!(item.tooltip, "line one");
    assert_eq!(item.badge.as_deref(), Some("3"));
    assert_eq!(item.menu[0].kind, MenuKind::Check);
    assert_eq!(item.activate, Activation::Event);
}

#[test]
fn limits_refuse_the_item() {
    let mut long = valid();
    long.tooltip = "x".repeat(item::TOOLTIP_MAX + 1);
    assert_eq!(Item::from_wire(long), Err(Invalid::Tooltip));
    let mut exact = valid();
    exact.tooltip = "é".repeat(item::TOOLTIP_MAX);
    assert!(Item::from_wire(exact).is_ok());
    let mut badge = valid();
    badge.badge = Some("1000".into());
    assert_eq!(Item::from_wire(badge), Err(Invalid::Badge));
    let mut status = valid();
    status.status = 3;
    assert_eq!(Item::from_wire(status), Err(Invalid::Status));
    let mut activate = valid();
    activate.activate = 9;
    assert_eq!(Item::from_wire(activate), Err(Invalid::Activation));
    let mut rows = valid();
    rows.menu = (1..=item::MENU_MAX as u32 + 1)
        .map(|id| row(id, 0, wire::MENU_KIND_NORMAL))
        .collect();
    assert_eq!(Item::from_wire(rows), Err(Invalid::MenuSize));
    let mut label = valid();
    label.menu[0].label = "y".repeat(item::LABEL_MAX + 1);
    assert_eq!(Item::from_wire(label), Err(Invalid::MenuLabel));
}

#[test]
fn menu_ids_parents_and_defaults_are_checked() {
    let menu = |rows: Vec<wire::MenuItem>| {
        Item::from_wire(wire::Item {
            menu: rows,
            ..valid()
        })
        .map(|item| item.menu.len())
    };
    let sub = wire::MENU_KIND_SUBMENU;
    let normal = wire::MENU_KIND_NORMAL;
    assert_eq!(menu(vec![row(1, 0, sub), row(2, 1, normal)]), Ok(2));
    assert_eq!(menu(vec![row(0, 0, normal)]), Err(Invalid::MenuId));
    assert_eq!(
        menu(vec![row(1, 0, normal), row(1, 0, normal)]),
        Err(Invalid::MenuId)
    );
    // A parent must come first, be a submenu, and not nest a submenu.
    assert_eq!(
        menu(vec![row(2, 1, normal), row(1, 0, sub)]),
        Err(Invalid::MenuParent)
    );
    assert_eq!(
        menu(vec![row(1, 0, normal), row(2, 1, normal)]),
        Err(Invalid::MenuParent)
    );
    assert_eq!(
        menu(vec![row(1, 0, sub), row(2, 1, sub)]),
        Err(Invalid::MenuParent)
    );
    assert_eq!(menu(vec![row(1, 0, 7)]), Err(Invalid::MenuKind));
    let mut first = row(1, 0, normal);
    first.is_default = true;
    let mut second = row(2, 0, normal);
    second.is_default = true;
    assert_eq!(menu(vec![first.clone(), second]), Err(Invalid::MenuDefault));
    let mut separator = row(3, 0, wire::MENU_KIND_SEPARATOR);
    separator.is_default = true;
    assert_eq!(menu(vec![separator]), Err(Invalid::MenuDefault));
    // `DefaultItem` needs its row.
    let mut runs_default = valid();
    runs_default.activate = wire::ACTIVATION_DEFAULT_ITEM;
    assert_eq!(
        Item::from_wire(runs_default.clone()),
        Err(Invalid::MenuDefault)
    );
    runs_default.menu = vec![first];
    assert!(Item::from_wire(runs_default).is_ok());
}

#[test]
fn unusable_icons_fall_back_instead_of_failing() {
    let icon_of = |icon: wire::Icon| {
        Item::from_wire(wire::Item { icon, ..valid() })
            .unwrap()
            .icon
    };
    assert_eq!(icon_of(wire::Icon::default()), None);
    assert_eq!(icon_of(lucide("Volume")), None);
    assert_eq!(
        icon_of(lucide(&"a".repeat(item::LUCIDE_NAME_MAX + 1))),
        None
    );
    let two = wire::Icon {
        file: Some("a.png".into()),
        ..lucide("x")
    };
    assert_eq!(icon_of(two), None);
    let mask = |img| wire::Icon {
        mask: Some(img),
        ..wire::Icon::default()
    };
    assert!(icon_of(mask(image(16, 16))).is_some());
    assert_eq!(icon_of(mask(image(65, 16))), None);
    assert_eq!(icon_of(mask(image(0, 16))), None);
    let mut short = image(16, 16);
    short.data.pop();
    assert_eq!(icon_of(mask(short)), None);
    let pixels = |images| wire::Icon {
        pixels: images,
        ..wire::Icon::default()
    };
    assert!(icon_of(pixels(vec![image(16, 16), image(32, 32)])).is_some());
    assert_eq!(icon_of(pixels(vec![image(16, 16); 3])), None);
    for bad in [
        "../x.png",
        "icons/x.png",
        "x.svg",
        ".png",
        ".hidden.png",
        "a b.png",
    ] {
        let package = wire::Icon {
            file: Some(bad.into()),
            ..wire::Icon::default()
        };
        assert_eq!(icon_of(package), None, "{bad}");
    }
    assert!(item::package_name("tray-mute.png"));
}

#[test]
fn the_fallback_chain_ends_in_app_window() {
    let known = |name: &str| name == "volume-2" || name == "app-window";
    let dir = Some("/apps/org.ex/1.0/icons");
    let volume = Source::Lucide("volume-2".into());
    assert_eq!(
        icon::chain(Some(&volume), 1, dir, known),
        vec![
            Picture::Lucide("volume-2"),
            Picture::File("/apps/org.ex/1.0/icons/app-16.png".into()),
            Picture::Lucide("app-window"),
        ]
    );
    // An unknown name skips straight to the package icon, at 2x the 32 one.
    let unknown = Source::Lucide("wifi-off".into());
    assert_eq!(
        icon::chain(Some(&unknown), 2, dir, known),
        vec![
            Picture::File("/apps/org.ex/1.0/icons/app-32.png".into()),
            Picture::Lucide("app-window"),
        ]
    );
    // A built-in without a package, and a package icon without a directory.
    let package = Source::Package("mute.png".into());
    assert_eq!(
        icon::chain(Some(&package), 1, None, known),
        vec![Picture::Lucide("app-window")]
    );
    assert_eq!(
        icon::chain(Some(&package), 1, Some("/i/"), known)[0],
        Picture::File("/i/mute.png".into())
    );
    assert_eq!(
        icon::chain(None, 1, None, known),
        vec![Picture::Lucide("app-window")]
    );
}

#[test]
fn pixels_pick_the_image_for_the_scale() {
    let images = [image(16, 16), image(32, 32)].map(|img| item::Image {
        width: img.width,
        height: img.height,
        data: img.data,
    });
    let pixels = Source::Pixels(images.to_vec());
    for (scale, side) in [(1, 16), (2, 32), (3, 32)] {
        match &icon::chain(Some(&pixels), scale, None, |_| false)[0] {
            Picture::Pixels(image) => assert_eq!(image.width, side),
            other => panic!("{other:?}"),
        }
    }
    assert!(icon::closest(&[], 16).is_none());
}

#[test]
fn set_update_and_clear_one_item_per_app() {
    let mut tray = Tray::new();
    assert_eq!(tray.set("org.a", item(Status::Active)), Ok(Change::Added));
    assert_eq!(
        tray.set("org.a", item(Status::Passive)),
        Ok(Change::Replaced)
    );
    assert_eq!(tray.len(), 1);
    let patch = Patch::from_wire(wire::UpdateArgs {
        tooltip: Some("hi".into()),
        badge: Some(String::new()),
        ..wire::UpdateArgs::default()
    })
    .unwrap();
    assert_eq!(tray.update("org.a", patch.clone()), Ok(Change::Replaced));
    let entry = tray.get("org.a").unwrap();
    assert_eq!(entry.tooltip(), "hi");
    assert_eq!(entry.status(), Status::Passive);
    assert_eq!(tray.update("org.b", patch), Err(Refused::NoItem));
    assert_eq!(tray.clear("org.a"), Change::Removed);
    assert_eq!(tray.clear("org.a"), Change::Unchanged);
    assert!(tray.is_empty());
    assert_eq!(tray.set("", Item::default()), Err(Refused::Key));
}

#[test]
fn an_update_that_breaks_default_activation_is_refused() {
    let mut tray = Tray::new();
    let mut wire_item = valid();
    wire_item.menu[0].is_default = true;
    wire_item.activate = wire::ACTIVATION_DEFAULT_ITEM;
    tray.set("org.a", Item::from_wire(wire_item).unwrap())
        .unwrap();
    let patch = Patch::from_wire(wire::UpdateArgs {
        menu: Some(wire::Menu { rows: Vec::new() }),
        ..wire::UpdateArgs::default()
    })
    .unwrap();
    assert_eq!(
        tray.update("org.a", patch),
        Err(Refused::Invalid(Invalid::MenuDefault))
    );
    assert_eq!(
        tray.get("org.a")
            .unwrap()
            .custom
            .as_ref()
            .unwrap()
            .menu
            .len(),
        1
    );
}

#[test]
fn resident_apps_keep_a_default_item_until_they_stop() {
    let mut tray = Tray::new();
    let running = vec![("org.vol".to_string(), 40)];
    assert_eq!(
        tray.set_resident(&running),
        vec![("org.vol".to_string(), Change::Added)]
    );
    assert!(tray.get("org.vol").unwrap().custom.is_none());
    assert_eq!(
        tray.set("org.vol", item(Status::Active)),
        Ok(Change::Replaced)
    );
    assert_eq!(tray.clear("org.vol"), Change::Reverted);
    assert_eq!(tray.get("org.vol").unwrap().resident, Some(40));
    // Republished unchanged: nothing moves.
    assert!(tray.set_resident(&running).is_empty());
    tray.set("org.vol", item(Status::Active)).unwrap();
    assert_eq!(
        tray.set_resident(&[]),
        vec![("org.vol".to_string(), Change::Removed)]
    );
    assert!(tray.is_empty());
}

#[test]
fn the_tray_holds_at_most_items_max() {
    let mut tray = Tray::new();
    for i in 0..ITEMS_MAX {
        tray.set(&format!("app{i}"), Item::default()).unwrap();
    }
    assert_eq!(tray.set("one-more", Item::default()), Err(Refused::Full));
    assert_eq!(tray.set("app3", Item::default()), Ok(Change::Replaced));
    assert!(tray.set_resident(&[("late".into(), 1)]).is_empty());
    assert_eq!(tray.len(), ITEMS_MAX);
}

#[test]
fn user_order_comes_first_then_first_appearance() {
    let mut tray = Tray::new();
    for app in ["a", "b", "c", "d"] {
        tray.set(app, Item::default()).unwrap();
    }
    tray.set_order(vec!["c".into(), "ghost".into(), "a".into()]);
    let order: Vec<&str> = tray.entries().iter().map(|e| e.app.as_str()).collect();
    assert_eq!(order, ["c", "a", "b", "d"]);
}

const SCREEN_W: i32 = 1280;
const CLOCK_X: i32 = 1180;

fn tray_of(statuses: &[Status]) -> Tray {
    let mut tray = Tray::new();
    for (i, status) in statuses.iter().enumerate() {
        tray.set(&format!("app{i}"), item(*status)).unwrap();
    }
    tray
}

#[test]
fn cells_sit_left_of_the_clock() {
    let tray = tray_of(&[Status::Active; 2]);
    let layout = layout::layout(&tray, CLOCK_X);
    assert_eq!(layout.chevron, None);
    assert_eq!(
        layout.cells[0].rect,
        Rect::new(CLOCK_X - 2 * CELL, 4, CELL, CELL)
    );
    assert_eq!(
        layout.cells[1].rect,
        Rect::new(CLOCK_X - CELL, 4, CELL, CELL)
    );
    assert_eq!(layout.reserved, 2 * CELL + TRAY_GAP);
    assert_eq!(layout.hit(CLOCK_X - 1, 10), Some(Hit::Item("app1")));
    assert_eq!(layout.hit(CLOCK_X - 1, 2), None);
    assert_eq!(
        layout::icon_rect(layout.cells[1].rect),
        Rect::new(CLOCK_X - 20, 8, 16, 16)
    );
    // The entries leave the clock and the tray free.
    let clock_w = SCREEN_W - CLOCK_X;
    let rects = taskbar::entry_rects(40, SCREEN_W, clock_w + layout.reserved);
    let right = rects.iter().flatten().map(|r| r.x + r.w).max().unwrap();
    assert!(right <= layout.cells[0].rect.x - TRAY_GAP);
    assert_eq!(layout::layout(&Tray::new(), CLOCK_X).reserved, 0);
}

#[test]
fn layout_scales_to_screen_pixels_at_2x() {
    // The shell multiplies design pixels by its scale at the protocol edge:
    // a 24 dp cell is 48 px and its icon 32 px at 2x.
    let tray = tray_of(&[Status::Active]);
    let cell = layout::layout(&tray, 1180).cells[0].rect;
    let icon = layout::icon_rect(cell);
    for scale in [1, 2] {
        let s = |v: i32| v * scale;
        assert_eq!(s(cell.w), 24 * scale);
        assert_eq!(s(icon.w), 16 * scale);
        assert_eq!(s(icon.x) - s(cell.x), 4 * scale);
        assert_eq!(s(cell.y) + s(cell.h) + s(4), s(crate::taskbar::BAR_H));
    }
}

#[test]
fn more_than_six_items_overflow_passive_ones_first() {
    let mut statuses = [Status::Active; 8];
    statuses[1] = Status::Passive;
    statuses[6] = Status::Attention;
    let tray = tray_of(&statuses);
    let layout = layout::layout(&tray, CLOCK_X);
    assert_eq!(layout.cells.len(), TRAY_VISIBLE);
    let on_bar: Vec<&str> = layout.cells.iter().map(|c| c.app.as_str()).collect();
    assert_eq!(on_bar, ["app0", "app2", "app3", "app4", "app5", "app6"]);
    assert_eq!(layout.overflow, ["app1", "app7"]);
    let chevron = layout.chevron.unwrap();
    assert_eq!(chevron.w, CHEVRON_W);
    assert_eq!(chevron.x + CHEVRON_W, layout.cells[0].rect.x);
    assert_eq!(layout.hit(chevron.x, 10), Some(Hit::Chevron));
    assert!(!layout.visible("app1"));
}

#[test]
fn hidden_items_go_to_the_overflow_and_narrow_bars_shed_cells() {
    let mut tray = tray_of(&[Status::Active; 3]);
    tray.set_hidden(vec!["app1".into()]);
    let layout = layout::layout(&tray, CLOCK_X);
    assert_eq!(layout.cells.len(), 2);
    assert_eq!(layout.overflow, ["app1"]);
    assert!(layout.chevron.is_some());
    // Room for the chevron and one cell only, right of the start button.
    let narrow = ENTRY_X + TRAY_GAP + CHEVRON_W + CELL;
    let layout = layout::layout(&tray, narrow);
    assert_eq!(layout.cells.len(), 1);
    assert_eq!(layout.overflow.len(), 2);
    assert!(layout.chevron.unwrap().x >= ENTRY_X);
    let layout = layout::layout(&tray, ENTRY_X);
    assert!(layout.cells.is_empty());
}

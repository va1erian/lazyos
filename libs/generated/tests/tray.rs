//! Round-trip tests for the tray and app-lifecycle stubs (docs/tray-plan.md
//! section 4): `os.lazy.shell.tray.v1`, its events, `os.lazy.init.app.v1` and
//! its events, and the resident-apps topic.

use messenger_generated::os_lazy_init_app_events_v1 as app_events;
use messenger_generated::os_lazy_init_app_v1 as app;
use messenger_generated::os_lazy_init_v1 as init;
use messenger_generated::os_lazy_shell_tray_events_v1 as events;
use messenger_generated::os_lazy_shell_tray_v1::*;

fn image(side: u32) -> Image {
    Image {
        width: side,
        height: side,
        data: vec![0x5a; (side * side * 4) as usize],
    }
}

fn menu() -> Vec<MenuItem> {
    vec![
        MenuItem {
            id: 1,
            parent: 0,
            label: "Mute".into(),
            kind: MENU_KIND_CHECK,
            enabled: true,
            checked: true,
            is_default: false,
        },
        MenuItem {
            id: 2,
            kind: MENU_KIND_SEPARATOR,
            ..MenuItem::default()
        },
        MenuItem {
            id: 3,
            parent: 0,
            label: "Output".into(),
            kind: MENU_KIND_SUBMENU,
            enabled: true,
            checked: false,
            is_default: true,
        },
    ]
}

#[test]
fn set_item_roundtrips_every_icon_source() {
    let icons = [
        Icon::default(),
        Icon {
            lucide: Some("volume-2".into()),
            ..Icon::default()
        },
        Icon {
            mask: Some(image(16)),
            ..Icon::default()
        },
        Icon {
            pixels: vec![image(16), image(32)],
            ..Icon::default()
        },
        Icon {
            file: Some("app-16.png".into()),
            ..Icon::default()
        },
    ];
    for icon in icons {
        let args = SetArgs {
            item: Item {
                icon,
                tooltip: "Volume 40%".into(),
                status: STATUS_ATTENTION,
                badge: Some("3".into()),
                menu: menu(),
                activate: ACTIVATION_DEFAULT_ITEM,
            },
            events: 5,
        };
        let (body, objects) = encode_set_args(&args).unwrap();
        assert_eq!(objects, vec![libmessenger::Object::Channel(5)]);
        let back = decode_set_args(&body, &[libmessenger::Object::Channel(12)]).unwrap();
        assert_eq!(back.item, args.item);
        assert_eq!(back.events, 12);
    }
    assert_eq!(SET_OBJECTS, &[libmessenger::ObjectKind::Channel]);
}

#[test]
fn update_keeps_absent_fields_absent() {
    let empty = UpdateArgs::default();
    assert_eq!(
        decode_update_args(&encode_update_args(&empty).unwrap()).unwrap(),
        empty
    );
    let full = UpdateArgs {
        icon: Some(Icon {
            lucide: Some("wifi".into()),
            ..Icon::default()
        }),
        tooltip: Some(String::new()),
        status: Some(STATUS_PASSIVE),
        badge: Some("99+".into()),
        menu: Some(Menu { rows: Vec::new() }),
    };
    assert_eq!(
        decode_update_args(&encode_update_args(&full).unwrap()).unwrap(),
        full
    );
}

#[test]
fn truncated_bodies_are_refused() {
    let body = encode_item(&Item {
        icon: Icon {
            pixels: vec![image(4)],
            ..Icon::default()
        },
        menu: menu(),
        ..Item::default()
    })
    .unwrap();
    for cut in 1..body.len() {
        // Never a panic; a cut inside a field header or payload is an error.
        let _ = decode_item(&body[..cut]);
    }
    assert!(decode_item(&body[..body.len() - 1]).is_err());
}

#[test]
fn events_roundtrip() {
    let anchor = events::Rect {
        x: -4,
        y: 700,
        w: 24,
        h: 24,
    };
    let activate = events::ActivateArgs {
        anchor: anchor.clone(),
        popup: u64::MAX,
    };
    let body = events::encode_activate_args(&activate).unwrap();
    assert_eq!(events::decode_activate_args(&body).unwrap(), activate);
    let item = events::MenuItemArgs {
        id: 7,
        checked: true,
    };
    let body = events::encode_menu_item_args(&item).unwrap();
    assert_eq!(events::decode_menu_item_args(&body).unwrap(), item);
    let scroll = events::ScrollArgs { delta: -3 };
    let body = events::encode_scroll_args(&scroll).unwrap();
    assert_eq!(events::decode_scroll_args(&body).unwrap(), scroll);
    assert_ne!(events::INTERFACE_ID, INTERFACE_ID);
}

// Both state topics are retained, so a late subscriber reads the value.
const _: () = assert!(TOPIC_SESSION_SHELL_TRAY_RETAINED);
const _: () = assert!(init::TOPIC_SESSION_APPS_RESIDENT_RETAINED);

#[test]
fn generation_topic_names_one_session() {
    assert_eq!(TOPIC_SESSION_SHELL_TRAY, "session/+/shell/tray");
    assert_eq!(
        name_session_shell_tray("3").unwrap(),
        "session/3/shell/tray"
    );
    assert!(name_session_shell_tray("+").is_err());
    let value = Generation { generation: 9 };
    let body = encode_session_shell_tray(&value).unwrap();
    assert_eq!(decode_session_shell_tray(&body).unwrap(), value);
}

#[test]
fn lifecycle_events_roundtrip() {
    let reopen = app_events::ReopenArgs {
        args: "/home/user/a.txt".into(),
    };
    let body = app_events::encode_reopen_args(&reopen).unwrap();
    assert_eq!(app_events::decode_reopen_args(&body).unwrap(), reopen);
    let quit = app_events::QuitArgs { grace_ms: 3000 };
    let body = app_events::encode_quit_args(&quit).unwrap();
    assert_eq!(app_events::decode_quit_args(&body).unwrap(), quit);
    assert_eq!(app::WATCH_OBJECTS, &[libmessenger::ObjectKind::Channel]);
    assert_ne!(app::INTERFACE_ID, init::INTERFACE_ID);
}

#[test]
fn resident_topic_lists_apps() {
    assert_eq!(init::TOPIC_SESSION_APPS_RESIDENT, "session/+/apps/resident");
    for count in [0u64, 1, 64] {
        let value = init::ResidentApps {
            apps: (0..count)
                .map(|i| init::ResidentApp {
                    app: format!("org.example.app{i}"),
                    pid: 100 + i,
                })
                .collect(),
        };
        let body = init::encode_session_apps_resident(&value).unwrap();
        assert_eq!(init::decode_session_apps_resident(&body).unwrap(), value);
    }
}

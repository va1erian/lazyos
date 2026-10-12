use super::*;
use crate::model::Link;

fn net(ssid: &str, rssi: i32, secured: bool, saved: &str) -> NetworkRow {
    NetworkRow {
        ssid: ssid.into(),
        secured,
        supported: true,
        bars: bars_of(rssi),
        rssi_dbm: rssi,
        saved_id: saved.into(),
        connected: false,
    }
}

/// The usual thresholds, only for the fixtures.
fn bars_of(rssi: i32) -> u8 {
    match rssi {
        r if r >= -55 => 4,
        r if r >= -67 => 3,
        r if r >= -75 => 2,
        r if r >= -85 => 1,
        _ => 0,
    }
}

fn idle() -> Link {
    Link {
        state: State::Idle,
        ..Link::unavailable()
    }
}

fn labels(menu: &Menu) -> Vec<&str> {
    menu.rows.iter().map(|r| r.label.as_str()).collect()
}

#[test]
fn networks_are_listed_strongest_first_with_lock_and_bars() {
    let heard = [
        net("Weak", -80, true, ""),
        net("LazyOpen", -50, false, ""),
        net("LazyWPA2", -62, true, "w1"),
    ];
    let menu = build(&idle(), &heard);
    let rows = labels(&menu);
    assert_eq!(rows[0], "Not connected");
    assert_eq!(rows[2], "LazyOpen  (open, 4 of 4 bars)");
    assert_eq!(rows[3], "LazyWPA2  (locked, 3 of 4 bars, saved)");
    assert_eq!(rows[4], "Weak  (locked, 1 of 4 bars)");
    assert_eq!(
        &rows[6..],
        ["Search again", "Disconnect", "Network settings..."]
    );
}

#[test]
fn at_most_eight_named_networks_are_listed() {
    let mut heard: Vec<NetworkRow> = (0..12)
        .map(|i| net(&format!("net{i:02}"), -40 - i, false, ""))
        .collect();
    heard.push(net("", -30, false, ""));
    let menu = build(&idle(), &heard);
    let listed = menu
        .rows
        .iter()
        .filter(|r| r.id >= NETWORK_BASE && r.id < RESCAN - 1)
        .count();
    assert_eq!(listed, MAX_NETWORKS);
    assert!(!labels(&menu).iter().any(|l| l.contains("net08")), "weakest dropped");
    assert!(!labels(&menu).iter().any(|l| l.starts_with("  (")), "hidden skipped");
}

#[test]
fn the_network_in_use_is_checked_and_does_nothing() {
    let mut current = net("LazyOpen", -50, false, "w1");
    current.connected = true;
    let link = Link {
        state: State::Connected,
        ssid: "LazyOpen".into(),
        ..Link::unavailable()
    };
    let menu = build(&link, &[current, net("Other", -70, false, "")]);
    assert_eq!(menu.rows[0].label, "Connected to LazyOpen");
    let row = menu.rows.iter().find(|r| r.id == NETWORK_BASE).unwrap();
    assert!(row.checked && row.kind == wire::MENU_KIND_CHECK);
    assert_eq!(menu.action(NETWORK_BASE), Action::None);
    let other = menu.rows.iter().find(|r| r.id == NETWORK_BASE + 1).unwrap();
    assert!(!other.checked && other.kind == wire::MENU_KIND_NORMAL);
}

#[test]
fn a_click_decides_by_what_is_known_of_the_network() {
    let heard = [
        net("Saved", -40, true, "w9"),
        net("OpenNew", -50, false, ""),
        net("SecuredNew", -60, true, ""),
        NetworkRow {
            supported: false,
            ..net("Enterprise", -65, true, "")
        },
    ];
    let menu = build(&idle(), &heard);
    assert_eq!(menu.action(NETWORK_BASE), Action::Connect { id: "w9".into() });
    assert_eq!(
        menu.action(NETWORK_BASE + 1),
        Action::JoinOpen {
            ssid: "OpenNew".into()
        }
    );
    assert_eq!(menu.action(NETWORK_BASE + 2), Action::Configure);
    assert_eq!(menu.action(NETWORK_BASE + 3), Action::None);
    let unsupported = menu.rows.iter().find(|r| r.id == NETWORK_BASE + 3).unwrap();
    assert!(!unsupported.enabled);
    assert!(unsupported.label.contains("not supported"));
    assert_eq!(menu.action(RESCAN), Action::Rescan);
    assert_eq!(menu.action(DISCONNECT), Action::Disconnect);
    assert_eq!(menu.action(SETTINGS), Action::Settings);
    assert_eq!(menu.action(4242), Action::None, "an id the menu never had");
}

#[test]
fn disconnect_is_offered_only_while_joined_or_joining() {
    for (state, enabled) in [
        (State::Connected, true),
        (State::Joining, true),
        (State::Idle, false),
        (State::Searching, false),
        (State::Failed, false),
        (State::Unavailable, false),
    ] {
        let link = Link {
            state,
            ..Link::unavailable()
        };
        let menu = build(&link, &[]);
        let row = menu.rows.iter().find(|r| r.id == DISCONNECT).unwrap();
        assert_eq!(row.enabled, enabled, "{state:?}");
    }
}

#[test]
fn an_empty_neighbourhood_says_so() {
    let menu = build(&idle(), &[]);
    assert_eq!(labels(&menu)[2], "No networks found");
    assert!(!menu.rows[2].enabled);
}

#[test]
fn the_menu_is_valid_for_the_tray() {
    // `os.lazy.shell.tray.v1`: at most 64 rows of at most 128 characters,
    // unique non-zero ids.
    let heard: Vec<NetworkRow> = (0..20)
        .map(|i| net(&"w".repeat(100 + i), -40 - i as i32, i % 2 == 0, ""))
        .collect();
    let menu = build(&idle(), &heard);
    assert!(menu.rows.len() <= 64);
    let mut ids: Vec<u32> = menu.rows.iter().map(|r| r.id).collect();
    assert!(ids.iter().all(|id| *id != 0));
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), menu.rows.len(), "ids are unique");
    for row in &menu.rows {
        assert!(row.label.chars().count() <= 128, "{}", row.label);
        assert_eq!(row.parent, 0);
    }
}

#[test]
fn a_hostile_name_cannot_inject_control_characters() {
    let heard = [net("evil\u{7}\nname", -40, false, "")];
    let menu = build(&idle(), &heard);
    assert!(menu.rows[2].label.starts_with("evilname  ("));
    assert!(menu.rows.iter().all(|r| !r.label.chars().any(char::is_control)));
}

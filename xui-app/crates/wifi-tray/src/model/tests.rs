use super::*;

fn connected() -> Link {
    Link {
        state: State::Connected,
        ssid: "LazyWPA2".into(),
        bars: 3,
        rssi_dbm: -61,
        band: "2.4 GHz",
        failure: None,
        address: Some([192, 168, 77, 20]),
    }
}

fn link(state: State) -> Link {
    Link {
        state,
        ..connected()
    }
}

#[test]
fn a_connection_shows_its_bars_and_everything_the_tooltip_knows() {
    let shown = show(&connected(), 0);
    assert_eq!(shown.glyph, Glyph::Bars(3));
    assert_eq!(shown.status, wire::STATUS_ACTIVE);
    assert_eq!(
        shown.tooltip,
        "Wi-Fi: connected to LazyWPA2, signal good (-61 dBm), 2.4 GHz, 192.168.77.20"
    );
}

#[test]
fn a_connection_without_measurements_still_says_something() {
    let bare = Link {
        rssi_dbm: 0,
        band: "",
        address: None,
        bars: 0,
        ..connected()
    };
    let shown = show(&bare, 0);
    assert_eq!(
        shown.tooltip,
        "Wi-Fi: connected to LazyWPA2, signal very weak"
    );
    assert_eq!(shown.glyph, Glyph::Bars(1), "never the disconnected look");
}

#[test]
fn every_state_has_its_own_icon_and_words() {
    let cases = [
        (
            State::Unavailable,
            Glyph::Unavailable,
            "Wi-Fi: not available",
        ),
        (State::Idle, Glyph::Disconnected, "Wi-Fi: not connected"),
        (
            State::Searching,
            Glyph::Disconnected,
            "Wi-Fi: searching for networks",
        ),
        (
            State::Joining,
            Glyph::Connecting(1),
            "Wi-Fi: connecting to LazyWPA2...",
        ),
    ];
    for (state, glyph, tooltip) in cases {
        let shown = show(&link(state), 1);
        assert_eq!((shown.glyph, shown.tooltip.as_str()), (glyph, tooltip));
    }
}

#[test]
fn a_failure_names_its_reason() {
    let failed = Link {
        failure: Some("the password is probably wrong".into()),
        ..link(State::Failed)
    };
    let shown = show(&failed, 0);
    assert_eq!(
        shown.tooltip,
        "Wi-Fi: not connected (the password is probably wrong)"
    );
    assert_eq!(shown.glyph, Glyph::Disconnected);
    assert_eq!(
        show(&link(State::Failed), 0).tooltip,
        "Wi-Fi: not connected"
    );
}

#[test]
fn joining_animates_and_asks_for_attention() {
    let joining = link(State::Joining);
    let phases: Vec<Glyph> = (0..8).map(|p| show(&joining, p).glyph).collect();
    assert_eq!(phases[0], Glyph::Connecting(0));
    assert_eq!(phases[3], Glyph::Connecting(3));
    assert_eq!(phases[4], Glyph::Connecting(0), "the phase wraps");
    assert_eq!(show(&joining, 0).status, wire::STATUS_ATTENTION);
    let nameless = Link {
        ssid: String::new(),
        ..joining
    };
    assert_eq!(show(&nameless, 0).tooltip, "Wi-Fi: connecting...");
}

#[test]
fn only_a_settled_connection_is_active() {
    for state in [
        State::Unavailable,
        State::Idle,
        State::Searching,
        State::Failed,
    ] {
        assert_eq!(
            show(&link(state), 0).status,
            wire::STATUS_PASSIVE,
            "{state:?}"
        );
    }
    assert_eq!(show(&link(State::Connected), 0).status, wire::STATUS_ACTIVE);
}

#[test]
fn signal_words_cover_every_bar_count() {
    let words: Vec<&str> = (0..=4).map(quality).collect();
    assert_eq!(words, ["very weak", "weak", "fair", "good", "excellent"]);
}

#[test]
fn a_wired_only_machine_keeps_the_link_icon() {
    assert!(!prefer_wifi(None, true));
    assert!(!prefer_wifi(None, false));
}

#[test]
fn a_cable_wins_over_an_idle_radio_but_not_over_a_connection() {
    let idle = link(State::Idle);
    assert!(!prefer_wifi(Some(&idle), true), "the cable carries traffic");
    assert!(prefer_wifi(Some(&idle), false), "nothing else is up");
    assert!(prefer_wifi(Some(&link(State::Connected)), true));
    assert!(prefer_wifi(Some(&link(State::Joining)), true));
    assert!(prefer_wifi(Some(&Link::unavailable()), false));
    assert!(!prefer_wifi(Some(&Link::unavailable()), true));
}

#[test]
fn tooltips_fit_the_tray() {
    let long = Link {
        ssid: "x".repeat(64),
        ..connected()
    };
    assert!(show(&long, 0).tooltip.chars().count() <= 256);
}

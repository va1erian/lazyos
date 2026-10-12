//! Tests of `wifiview` (host, pure).

use super::*;

fn net(ssid: &str, security: Security, id: &str) -> Network {
    Network {
        ssid: ssid.into(),
        security,
        rssi_dbm: -58,
        freq_mhz: 5180,
        bss_count: 1,
        id: id.into(),
        connected: false,
    }
}

#[test]
fn passphrases_follow_the_service_rule() {
    assert_eq!(secret_problem("lazy-wpa2-pass"), None);
    assert_eq!(secret_problem("12345678"), None);
    assert_eq!(secret_problem(&"x".repeat(63)), None);
    assert_eq!(
        secret_problem(&"a".repeat(64)),
        None,
        "64 hex digits are a key"
    );
    assert_eq!(secret_problem(&"F0".repeat(32)), None);
    assert_eq!(secret_problem("caf\u{e9} au lait"), None, "UTF-8 is fine");
    assert!(secret_problem("short").is_some());
    assert!(secret_problem("").is_some());
    assert!(
        secret_problem(&"x".repeat(64)).is_some(),
        "64 non-hex is too long"
    );
    assert!(secret_problem(&"x".repeat(100)).is_some());
    assert!(secret_problem("pass\nword!").is_some());
    assert!(secret_problem("pass\u{7}word!").is_some());
    // 22 euro signs are 66 octets: over the limit although only 22 chars.
    assert!(secret_problem(&"\u{20ac}".repeat(22)).is_some());
    assert_eq!(secret_problem(&"\u{20ac}".repeat(21)), None);
}

#[test]
fn ssids_are_one_to_thirty_two_octets() {
    assert_eq!(ssid_problem("LazyWPA2"), None);
    assert_eq!(ssid_problem(&"s".repeat(32)), None);
    assert!(ssid_problem("").is_some());
    assert!(ssid_problem(&"s".repeat(33)).is_some());
    assert!(ssid_problem("a\tb").is_some());
}

#[test]
fn choosing_a_network_depends_on_what_is_saved() {
    assert_eq!(
        plan(&net("Home", Security::Wpa2Psk, "w0123456789ab")),
        Plan::Connect {
            id: "w0123456789ab".into()
        }
    );
    assert_eq!(
        plan(&net("Cafe", Security::Open, "")),
        Plan::AddOpen {
            ssid: "Cafe".into()
        }
    );
    assert_eq!(
        plan(&net("LazyWPA2", Security::Wpa2Psk, "")),
        Plan::AskPassword {
            ssid: "LazyWPA2".into()
        }
    );
    assert_eq!(plan(&net("", Security::Open, "")), Plan::Hidden);
    assert_eq!(
        plan(&net("Corp", Security::Unsupported, "")),
        Plan::Unsupported
    );
}

#[test]
fn rows_show_text_only_and_mark_the_connected_one() {
    let mut home = net("Home", Security::Wpa2Psk, "w0123456789ab");
    home.connected = true;
    let list = [
        home,
        net("", Security::Open, ""),
        net("<b>x</b>", Security::Open, ""),
    ];
    let shown = rows(&list);
    assert_eq!(shown[0].cells[0], "Home");
    assert_eq!(shown[0].cells[1], "WPA2");
    assert_eq!(shown[0].cells[2], "|||. -58 dBm");
    assert_eq!(shown[0].cells[3], "5 GHz");
    assert_eq!(shown[0].cells[4], "Connected");
    assert!(shown[0].locked);
    assert_eq!(shown[1].cells[0], HIDDEN_NAME);
    assert!(!shown[1].locked);
    // Markup stays what it is: text.
    assert_eq!(shown[2].cells[0], "<b>x</b>");
    let mut saved = net("Old", Security::Open, "w000000000001");
    saved.rssi_dbm = 0;
    assert_eq!(row(&saved).cells[4], "Saved");
    assert_eq!(row(&saved).cells[2], "-");
}

#[test]
fn the_selection_follows_the_name_then_the_connection() {
    let mut b = net("B", Security::Open, "");
    b.connected = true;
    let list = [
        net("A", Security::Open, ""),
        b,
        net("C", Security::Open, ""),
    ];
    assert_eq!(selection_after(&list, Some("C")), Some(2));
    assert_eq!(selection_after(&list, Some("gone")), Some(1));
    assert_eq!(selection_after(&list, None), Some(1));
    assert_eq!(selection_after(&list[..1], None), Some(0));
    assert_eq!(selection_after(&[], Some("A")), None);
}

fn status(state: ConnState, ssid: &str, reason: u32) -> Status {
    Status {
        state,
        ssid: ssid.into(),
        rssi_dbm: -58,
        freq_mhz: 5180,
        id: String::new(),
        reason,
    }
}

#[test]
fn the_status_line_tells_state_network_and_reason() {
    assert_eq!(
        status_line(&status(ConnState::Connected, "LazyWPA2", 0)),
        "Connected: LazyWPA2 (5 GHz, -58 dBm)"
    );
    assert_eq!(
        status_line(&status(ConnState::Associating, "LazyWPA2", 0)),
        "Connecting: LazyWPA2"
    );
    assert_eq!(
        status_line(&status(ConnState::Idle, "", 0)),
        "Not connected"
    );
    let bad = wifi::reason_text(3).expect("a reason");
    assert_eq!(
        status_line(&status(ConnState::Failed, "LazyWPA2", 3)),
        format!("Not connected: LazyWPA2 ({bad})")
    );
    assert_eq!(
        status_line(&status(ConnState::Failed, "", 0)),
        "Not connected"
    );
    assert!(status_line(&status(ConnState::Disabled, "", 0)).contains("radio"));
}

#[test]
fn a_known_network_needs_only_its_password() {
    let kind = JoinKind::Known {
        ssid: "LazyWPA2".into(),
    };
    let join = check_join(&kind, "ignored", false, "lazy-wpa2-pass").unwrap();
    assert_eq!(
        join,
        Join {
            ssid: "LazyWPA2".into(),
            security: Security::Wpa2Psk,
            hidden: false
        }
    );
    assert!(check_join(&kind, "", false, "short").is_err());
    assert!(check_join(&kind, "", false, "").is_err());
}

#[test]
fn the_other_network_form_checks_name_security_and_password() {
    let other = JoinKind::Other;
    assert!(check_join(&other, "", true, "lazy-wpa2-pass").is_err());
    assert!(check_join(&other, "Hidden", true, "short").is_err());
    let wpa2 = check_join(&other, "Hidden", true, "lazy-wpa2-pass").unwrap();
    assert_eq!(wpa2.security, Security::Wpa2Psk);
    assert!(wpa2.hidden, "a typed name may be hidden");
    // An open network ignores whatever is in the password field.
    let open = check_join(&other, "Cafe", false, "x").unwrap();
    assert_eq!(open.security, Security::Open);
}

#[test]
fn elevd_gets_the_password_only_for_wpa2() {
    let wpa2 = Join {
        ssid: "Lazy".into(),
        security: Security::Wpa2Psk,
        hidden: true,
    };
    assert_eq!(
        elevd_add_args(&wpa2, "lazy-wpa2-pass"),
        ["Lazy", "wpa2", "1", "lazy-wpa2-pass"]
    );
    let open = Join {
        ssid: "Cafe".into(),
        security: Security::Open,
        hidden: false,
    };
    assert_eq!(
        elevd_add_args(&open, "left over"),
        ["Cafe", "open", "0", ""]
    );
}

#[test]
fn scans_are_paced_and_paused_while_joining() {
    let mut pacer = ScanPacer::default();
    assert!(pacer.tick(false), "first tick scans");
    for _ in 0..SCAN_EVERY - 1 {
        assert!(!pacer.tick(false));
    }
    assert!(pacer.tick(false), "then every SCAN_EVERY ticks");
    assert!(!pacer.tick(true), "never while joining");
    pacer.retry_soon();
    assert!(pacer.tick(false), "a busy answer is retried at once");
    pacer.restart();
    assert!(pacer.tick(false), "reopening the page scans again");
}

#[test]
fn bars_are_four_wide() {
    assert_eq!(bars_text(0), "....");
    assert_eq!(bars_text(3), "|||.");
    assert_eq!(bars_text(9), "||||");
}

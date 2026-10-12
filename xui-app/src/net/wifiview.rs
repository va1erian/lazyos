//! The Network app's Wi-Fi page, the parts with no window in them (pure,
//! host tested): how a heard network is listed, what choosing it does, the
//! join form's checks, the status line and how often to scan.
//!
//! Nothing here keeps a password. [`check_join`] only looks at the text it is
//! handed and says whether it would do; the app passes the same `String` on
//! to [`super::wifi::add_network`] (which wipes it) or to `elevd` and clears
//! the field.

use super::wifi::{self, ConnState, Network, Security, Status};

/// Fewest octets of a WPA2 passphrase.
pub const SECRET_MIN: usize = 8;
/// Most octets of a WPA2 passphrase (64 hex digits are a raw key instead).
pub const SECRET_MAX: usize = 63;
/// Longest SSID, in octets.
pub const SSID_MAX: usize = 32;
/// Ticks (the app's one-second refresh) between two scans while the page is
/// shown and nothing is being joined.
pub const SCAN_EVERY: u32 = 10;
/// What a hidden network that is not saved is called in the list.
pub const HIDDEN_NAME: &str = "(hidden network)";

/// Why `secret` cannot be a WPA2 passphrase, or `None` when it can: 8 to 63
/// octets of text without control characters, or exactly 64 hex digits (a
/// raw key). It is `wlanmd`'s rule, so a refusal there stays rare.
pub fn secret_problem(secret: &str) -> Option<&'static str> {
    if secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if secret.chars().any(char::is_control) {
        return Some("The password cannot contain control characters.");
    }
    if secret.len() < SECRET_MIN {
        return Some("The password needs at least 8 characters.");
    }
    if secret.len() > SECRET_MAX {
        return Some("The password is too long (63 bytes, or exactly 64 hex digits).");
    }
    None
}

/// Why `name` cannot be the SSID of a network to join, or `None`.
pub fn ssid_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Type the network name.");
    }
    if name.len() > SSID_MAX {
        return Some("A network name is 32 bytes at most.");
    }
    if name.chars().any(char::is_control) {
        return Some("The network name cannot contain control characters.");
    }
    None
}

/// What choosing a listed network does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Saved: join it.
    Connect { id: String },
    /// Not saved and open: save it, then join.
    AddOpen { ssid: String },
    /// Not saved and WPA2: ask for the password first.
    AskPassword { ssid: String },
    /// Heard without a name: the "Other network" form joins it.
    Hidden,
    /// A security mode the station cannot join.
    Unsupported,
}

/// What choosing `network` does.
pub fn plan(network: &Network) -> Plan {
    if !network.id.is_empty() {
        return Plan::Connect {
            id: network.id.clone(),
        };
    }
    if network.ssid.is_empty() {
        return Plan::Hidden;
    }
    match network.security {
        Security::Open => Plan::AddOpen {
            ssid: network.ssid.clone(),
        },
        Security::Wpa2Psk => Plan::AskPassword {
            ssid: network.ssid.clone(),
        },
        Security::Unsupported => Plan::Unsupported,
    }
}

/// Signal bars as text: `|` for each bar lit, `.` for the rest of four.
pub fn bars_text(bars: u8) -> String {
    let lit = usize::from(bars.min(4));
    format!("{}{}", "|".repeat(lit), ".".repeat(4 - lit))
}

/// One list row: the cells and whether the network asks for a password
/// (it gets the lock icon).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Name, security, signal, band, state.
    pub cells: [String; 5],
    pub locked: bool,
}

/// The list rows for `networks`, in their order.
pub fn rows(networks: &[Network]) -> Vec<Row> {
    networks.iter().map(row).collect()
}

fn row(network: &Network) -> Row {
    let name = if network.ssid.is_empty() {
        String::from(HIDDEN_NAME)
    } else {
        network.ssid.clone()
    };
    let security = match network.security {
        Security::Open => "Open",
        Security::Wpa2Psk => "WPA2",
        Security::Unsupported => "Not supported",
    };
    let signal = if network.rssi_dbm == 0 {
        String::from("-")
    } else {
        format!("{} {} dBm", bars_text(network.bars()), network.rssi_dbm)
    };
    let state = if network.connected {
        "Connected"
    } else if !network.id.is_empty() {
        "Saved"
    } else {
        ""
    };
    Row {
        cells: [
            name,
            String::from(security),
            signal,
            String::from(wifi::band_label(network.freq_mhz)),
            String::from(state),
        ],
        locked: network.security.locked(),
    }
}

/// The index of the network named `ssid` (the selection that survives a
/// refresh), else of the connected one, else the first.
pub fn selection_after(networks: &[Network], keep: Option<&str>) -> Option<usize> {
    keep.and_then(|ssid| networks.iter().position(|n| n.ssid == ssid))
        .or_else(|| networks.iter().position(|n| n.connected))
        .or((!networks.is_empty()).then_some(0))
}

/// The status line: the state in words, the network and, when an attempt
/// failed, why.
pub fn status_line(status: &Status) -> String {
    let label = status.state.label();
    let named = if status.ssid.is_empty() {
        String::from(label)
    } else {
        format!("{label}: {}", status.ssid)
    };
    match status.state {
        ConnState::Connected => {
            let band = wifi::band_label(status.freq_mhz);
            let extra = match (band.is_empty(), status.rssi_dbm) {
                (true, 0) => String::new(),
                (true, dbm) => format!(" ({dbm} dBm)"),
                (false, 0) => format!(" ({band})"),
                (false, dbm) => format!(" ({band}, {dbm} dBm)"),
            };
            format!("{named}{extra}")
        }
        ConnState::Failed => match wifi::reason_text(status.reason) {
            Some(why) if status.ssid.is_empty() => format!("{label}: {why}"),
            Some(why) => format!("{named} ({why})"),
            None => named,
        },
        ConnState::Disabled => String::from("No Wi-Fi radio is available."),
        _ => named,
    }
}

/// What the join form is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinKind {
    /// A heard WPA2 network that is not saved; its name is fixed.
    Known { ssid: String },
    /// "Other network...": the name and security are typed.
    Other,
}

/// A checked join form: what to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Join {
    pub ssid: String,
    pub security: Security,
    /// Probe for the name (typed by hand, so it may not be on the air).
    pub hidden: bool,
}

/// Check the join form. `typed_ssid` and `wpa2` count only for
/// [`JoinKind::Other`]; `secret` only when the network is WPA2. The error is
/// the sentence to show.
pub fn check_join(
    kind: &JoinKind,
    typed_ssid: &str,
    wpa2: bool,
    secret: &str,
) -> Result<Join, &'static str> {
    let (ssid, security, hidden) = match kind {
        JoinKind::Known { ssid } => (ssid.clone(), Security::Wpa2Psk, false),
        JoinKind::Other => {
            if let Some(problem) = ssid_problem(typed_ssid) {
                return Err(problem);
            }
            let security = if wpa2 {
                Security::Wpa2Psk
            } else {
                Security::Open
            };
            (typed_ssid.to_string(), security, true)
        }
    };
    if security == Security::Wpa2Psk {
        if let Some(problem) = secret_problem(secret) {
            return Err(problem);
        }
    }
    Ok(Join {
        ssid,
        security,
        hidden,
    })
}

/// The arguments of `elevd`'s `net.wifi.add` (SSID, security, hidden,
/// passphrase) for a network available to all users.
pub fn elevd_add_args(join: &Join, secret: &str) -> [String; 4] {
    let wpa2 = join.security == Security::Wpa2Psk;
    [
        join.ssid.clone(),
        String::from(if wpa2 { "wpa2" } else { "open" }),
        String::from(if join.hidden { "1" } else { "0" }),
        if wpa2 {
            secret.to_string()
        } else {
            String::new()
        },
    ]
}

/// When to ask for a fresh scan: once on opening, then every
/// [`SCAN_EVERY`] ticks, never while a join is in progress (a scan pauses
/// traffic and may be skipped by the handshake), and again at once after a
/// busy answer.
#[derive(Debug, Default)]
pub struct ScanPacer {
    since: u32,
    started: bool,
}

impl ScanPacer {
    /// The page was just shown: scan at the next tick.
    pub fn restart(&mut self) {
        *self = ScanPacer::default();
    }

    /// One tick; whether a scan should be asked for now.
    pub fn tick(&mut self, joining: bool) -> bool {
        if joining {
            return false;
        }
        if !self.started {
            self.started = true;
            self.since = 0;
            return true;
        }
        self.since += 1;
        if self.since >= SCAN_EVERY {
            self.since = 0;
            return true;
        }
        false
    }

    /// The service was busy: ask again at the next tick.
    pub fn retry_soon(&mut self) {
        self.since = SCAN_EVERY;
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(secret_problem(&"a".repeat(64)), None, "64 hex digits are a key");
        assert_eq!(secret_problem(&"F0".repeat(32)), None);
        assert_eq!(secret_problem("caf\u{e9} au lait"), None, "UTF-8 is fine");
        assert!(secret_problem("short").is_some());
        assert!(secret_problem("").is_some());
        assert!(secret_problem(&"x".repeat(64)).is_some(), "64 non-hex is too long");
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
        assert_eq!(plan(&net("Corp", Security::Unsupported, "")), Plan::Unsupported);
    }

    #[test]
    fn rows_show_text_only_and_mark_the_connected_one() {
        let mut home = net("Home", Security::Wpa2Psk, "w0123456789ab");
        home.connected = true;
        let list = [home, net("", Security::Open, ""), net("<b>x</b>", Security::Open, "")];
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
        let list = [net("A", Security::Open, ""), b, net("C", Security::Open, "")];
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
        assert_eq!(status_line(&status(ConnState::Idle, "", 0)), "Not connected");
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
        assert_eq!(elevd_add_args(&open, "left over"), ["Cafe", "open", "0", ""]);
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
}

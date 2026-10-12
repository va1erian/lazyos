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
#[path = "wifiview_tests.rs"]
mod tests;

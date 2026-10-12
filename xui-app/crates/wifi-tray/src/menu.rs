//! The tray menu: the networks heard, strongest first, then the actions.
//!
//! Rows carry plain ASCII text (the SSID is the only free text; it is
//! clipped and stripped of control characters). What a row does is decided
//! here, from what is known of the network, so it is the same on the host
//! and in the guest and is tested without a radio:
//!
//! * the network in use does nothing (it is checked);
//! * a saved network connects (`wlanmd` `Connect`);
//! * an open network that is not saved is saved and connected;
//! * a secured network that is not saved needs a passphrase, which the tray
//!   never asks for: the Network app opens on its Wi-Fi page instead;
//! * a network of an unsupported security mode is listed and disabled.

use trayclient::wire;

use crate::model::{Link, State};

/// Most networks listed.
pub const MAX_NETWORKS: usize = 8;
/// Longest SSID shown in a row (characters).
const MAX_SSID: usize = 32;

/// Row ids. Networks take `NETWORK_BASE + index`.
pub const HEADER: u32 = 1;
pub const NETWORK_BASE: u32 = 10;
pub const RESCAN: u32 = 100;
pub const DISCONNECT: u32 = 101;
pub const SETTINGS: u32 = 102;

/// A heard network, reduced to what the menu needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkRow {
    pub ssid: String,
    /// Needs a passphrase.
    pub secured: bool,
    /// Its security mode can be joined (open or WPA2-Personal).
    pub supported: bool,
    pub bars: u8,
    pub rssi_dbm: i32,
    /// The saved profile's id when the caller may use it, else empty.
    pub saved_id: String,
    /// The network in use.
    pub connected: bool,
}

/// What a picked row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Join the saved network.
    Connect { id: String },
    /// Save the open network `ssid` for the user and join it.
    JoinOpen { ssid: String },
    /// Open the Network app on its Wi-Fi page: a passphrase is needed.
    Configure,
    /// Leave the network.
    Disconnect,
    /// Look for networks again.
    Rescan,
    /// Open the Network app.
    Settings,
    /// Nothing to do.
    None,
}

/// The menu's rows and what each does.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub rows: Vec<wire::MenuItem>,
    actions: Vec<(u32, Action)>,
}

impl Menu {
    /// What row `id` does ([`Action::None`] for an id the menu never had).
    pub fn action(&self, id: u32) -> Action {
        self.actions
            .iter()
            .find(|(known, _)| *known == id)
            .map_or(Action::None, |(_, action)| action.clone())
    }
}

/// Build the menu for `link` and the networks `heard`.
pub fn build(link: &Link, heard: &[NetworkRow]) -> Menu {
    let mut rows = vec![row(HEADER, &header(link), wire::MENU_KIND_NORMAL, false, false)];
    let mut actions = vec![(HEADER, Action::None)];
    rows.push(separator(HEADER + 1));
    let shown = listed(heard);
    if shown.is_empty() {
        rows.push(row(
            NETWORK_BASE,
            "No networks found",
            wire::MENU_KIND_NORMAL,
            false,
            false,
        ));
    }
    for (index, network) in shown.iter().enumerate() {
        let id = NETWORK_BASE + index as u32;
        let enabled = network.supported;
        // Only the network in use carries a check mark.
        let kind = if network.connected {
            wire::MENU_KIND_CHECK
        } else {
            wire::MENU_KIND_NORMAL
        };
        rows.push(row(id, &label(network), kind, enabled, network.connected));
        actions.push((id, pick(network)));
    }
    rows.push(separator(RESCAN - 1));
    rows.push(row(RESCAN, "Search again", wire::MENU_KIND_NORMAL, true, false));
    actions.push((RESCAN, Action::Rescan));
    let leaving = matches!(link.state, State::Connected | State::Joining);
    rows.push(row(
        DISCONNECT,
        "Disconnect",
        wire::MENU_KIND_NORMAL,
        leaving,
        false,
    ));
    actions.push((DISCONNECT, Action::Disconnect));
    rows.push(row(
        SETTINGS,
        "Network settings...",
        wire::MENU_KIND_NORMAL,
        true,
        false,
    ));
    actions.push((SETTINGS, Action::Settings));
    Menu { rows, actions }
}

/// The networks to list: named, strongest first, at most [`MAX_NETWORKS`].
fn listed(heard: &[NetworkRow]) -> Vec<NetworkRow> {
    let mut named: Vec<NetworkRow> = heard
        .iter()
        .filter(|network| !network.ssid.is_empty())
        .cloned()
        .collect();
    named.sort_by(|a, b| {
        b.rssi_dbm
            .cmp(&a.rssi_dbm)
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    named.truncate(MAX_NETWORKS);
    named
}

fn pick(network: &NetworkRow) -> Action {
    if !network.supported || network.connected {
        Action::None
    } else if !network.saved_id.is_empty() {
        Action::Connect {
            id: network.saved_id.clone(),
        }
    } else if network.secured {
        Action::Configure
    } else {
        Action::JoinOpen {
            ssid: network.ssid.clone(),
        }
    }
}

fn header(link: &Link) -> String {
    match link.state {
        State::Connected => format!("Connected to {}", clip(&link.ssid)),
        State::Joining if !link.ssid.is_empty() => format!("Connecting to {}", clip(&link.ssid)),
        State::Joining => String::from("Connecting"),
        State::Searching => String::from("Searching for networks"),
        State::Unavailable => String::from("Wi-Fi is not available"),
        State::Idle | State::Failed => String::from("Not connected"),
    }
}

/// `LazyWPA2  (locked, 3 of 4 bars, saved)`: the lock and the bars in words,
/// since a menu row is plain text.
fn label(network: &NetworkRow) -> String {
    let mut notes = vec![String::from(if !network.supported {
        "not supported"
    } else if network.secured {
        "locked"
    } else {
        "open"
    })];
    notes.push(format!("{} of 4 bars", network.bars.min(4)));
    if !network.saved_id.is_empty() {
        notes.push(String::from("saved"));
    }
    format!("{}  ({})", clip(&network.ssid), notes.join(", "))
}

fn clip(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_SSID)
        .collect()
}

fn row(id: u32, label: &str, kind: u32, enabled: bool, checked: bool) -> wire::MenuItem {
    wire::MenuItem {
        id,
        parent: 0,
        label: label.to_owned(),
        kind,
        enabled,
        checked,
        is_default: false,
    }
}

fn separator(id: u32) -> wire::MenuItem {
    row(id, "", wire::MENU_KIND_SEPARATOR, false, false)
}

#[cfg(test)]
mod tests;

//! From what the applet knows of the radio to what the tray shows.

use trayclient::wire;

use crate::glyph::Glyph;

/// The radio's state, as the applet groups `wlanmd`'s `ConnState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No usable radio: `wlanmd` is down, or the interface is `Disabled`.
    Unavailable,
    /// Idle: not joined and not looking.
    Idle,
    /// Looking for networks.
    Searching,
    /// Authenticating, associating or running the handshake.
    Joining,
    Connected,
    /// The last attempt failed.
    Failed,
}

/// What the applet knows of one wireless interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub state: State,
    /// The network joined or being joined (empty when none).
    pub ssid: String,
    /// Signal in bars, 0 to 4.
    pub bars: u8,
    /// Signal in dBm, 0 when not measured.
    pub rssi_dbm: i32,
    /// The band ("5 GHz"), empty when unknown.
    pub band: &'static str,
    /// Why the last attempt failed, in words.
    pub failure: Option<String>,
    /// The IPv4 address `netd` leased on the interface.
    pub address: Option<[u8; 4]>,
}

impl Link {
    /// A link with nothing known and nothing to show.
    pub fn unavailable() -> Link {
        Link {
            state: State::Unavailable,
            ssid: String::new(),
            bars: 0,
            rssi_dbm: 0,
            band: "",
            failure: None,
            address: None,
        }
    }

    /// Whether the radio is joined or joining something: the tray then
    /// shows it even when a cable carries the traffic.
    pub fn engaged(&self) -> bool {
        matches!(self.state, State::Connected | State::Joining)
    }
}

/// Whether the tray shows the Wi-Fi icon rather than the wired link one:
/// when a radio exists, unless a cable carries the connection and the radio
/// is idle. A wired-only machine has no `wifi` and keeps the link icon.
pub fn prefer_wifi(wifi: Option<&Link>, wired_up: bool) -> bool {
    wifi.is_some_and(|link| link.engaged() || !wired_up)
}

/// The item's look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub glyph: Glyph,
    pub tooltip: String,
    /// A `wire::STATUS_*` value.
    pub status: u32,
}

/// How good a signal of `bars` is, in a word.
pub fn quality(bars: u8) -> &'static str {
    match bars {
        4.. => "excellent",
        3 => "good",
        2 => "fair",
        1 => "weak",
        0 => "very weak",
    }
}

/// The glyph, tooltip and status for `link`. `phase` counts the applet's
/// looks, so a joining icon can animate (only the low two bits are used).
pub fn show(link: &Link, phase: u8) -> Shown {
    let (glyph, status, tooltip) = match link.state {
        State::Unavailable => (
            Glyph::Unavailable,
            wire::STATUS_PASSIVE,
            String::from("Wi-Fi: not available"),
        ),
        State::Idle => (
            Glyph::Disconnected,
            wire::STATUS_PASSIVE,
            String::from("Wi-Fi: not connected"),
        ),
        State::Searching => (
            Glyph::Disconnected,
            wire::STATUS_PASSIVE,
            String::from("Wi-Fi: searching for networks"),
        ),
        State::Failed => {
            let why = link
                .failure
                .as_deref()
                .map(|why| format!(" ({why})"))
                .unwrap_or_default();
            (
                Glyph::Disconnected,
                wire::STATUS_PASSIVE,
                format!("Wi-Fi: not connected{why}"),
            )
        }
        State::Joining => (
            Glyph::Connecting(phase & 3),
            wire::STATUS_ATTENTION,
            if link.ssid.is_empty() {
                String::from("Wi-Fi: connecting...")
            } else {
                format!("Wi-Fi: connecting to {}...", link.ssid)
            },
        ),
        State::Connected => (
            Glyph::connected(link.bars),
            wire::STATUS_ACTIVE,
            connected_text(link),
        ),
    };
    Shown {
        glyph,
        tooltip,
        status,
    }
}

fn connected_text(link: &Link) -> String {
    let mut text = format!("Wi-Fi: connected to {}", link.ssid);
    let mut detail = vec![format!("signal {}", quality(link.bars))];
    if link.rssi_dbm != 0 {
        detail[0] = format!("signal {} ({} dBm)", quality(link.bars), link.rssi_dbm);
    }
    if !link.band.is_empty() {
        detail.push(link.band.to_owned());
    }
    if let Some([a, b, c, d]) = link.address {
        detail.push(format!("{a}.{b}.{c}.{d}"));
    }
    text.push_str(", ");
    text.push_str(&detail.join(", "));
    text
}

#[cfg(test)]
mod tests;

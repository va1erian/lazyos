//! What the Network app's Wi-Fi page shows and checks, apart from any
//! service call (pure, host tested): `wlanmd`'s answers as table rows and
//! status lines, and the passphrase and name rules the form applies before
//! anything is sent (`AddNetwork` refuses the same inputs with `EINVAL`, but
//! a form that says why is kinder than one that says "invalid request").
//!
//! Everything that came from the air is untrusted text: `wlanmd` already
//! clips and strips names, and the client in [`super::wifi`] does it again,
//! so a label never sees a control character.

/// Longest SSID, in bytes (IEEE 802.11).
pub const MAX_SSID: usize = 32;
/// Shortest and longest WPA2 passphrase, in octets; a raw PSK is
/// [`PSK_HEX`] hex digits.
pub const MIN_PASSPHRASE: usize = 8;
pub const MAX_PASSPHRASE: usize = 63;
pub const PSK_HEX: usize = 64;

/// How a network is secured, as far as this client is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    Wpa2Psk,
    /// Something the station cannot join (WPA3, enterprise).
    Unsupported,
}

impl Security {
    pub fn text(self) -> &'static str {
        match self {
            Security::Open => "Open",
            Security::Wpa2Psk => "WPA2",
            Security::Unsupported => "Not supported",
        }
    }

    /// Whether joining a new network of this kind needs a passphrase.
    pub fn needs_passphrase(self) -> bool {
        self == Security::Wpa2Psk
    }

    pub fn joinable(self) -> bool {
        self != Security::Unsupported
    }
}

/// One heard network, merged with what is remembered about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// Empty for a hidden network that is not a remembered profile.
    pub ssid: String,
    pub security: Security,
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    pub bss_count: u32,
    /// The profile id when the caller may use it; empty otherwise.
    pub id: String,
    pub connected: bool,
}

impl Network {
    pub fn known(&self) -> bool {
        !self.id.is_empty()
    }

    /// The name to show: the SSID, or a placeholder for a hidden one.
    pub fn display_name(&self) -> &str {
        if self.ssid.is_empty() {
            "(hidden network)"
        } else {
            &self.ssid
        }
    }
}

/// `ConnState` of `os.lazy.net.wifi.v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Conn {
    #[default]
    Disabled,
    Idle,
    Scanning,
    Authenticating,
    Associating,
    Handshake,
    Connected,
    Failed,
}

impl Conn {
    /// The state's name for serial evidence and logs.
    pub fn name(self) -> &'static str {
        match self {
            Conn::Disabled => "disabled",
            Conn::Idle => "idle",
            Conn::Scanning => "scanning",
            Conn::Authenticating => "authenticating",
            Conn::Associating => "associating",
            Conn::Handshake => "handshake",
            Conn::Connected => "connected",
            Conn::Failed => "failed",
        }
    }
}

/// `FailReason` of `os.lazy.net.wifi.v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Why {
    #[default]
    None,
    NotFound,
    Rejected,
    BadPassphrase,
    Timeout,
    Deauthenticated,
    BeaconLoss,
    PolicyMismatch,
    Radio,
    Unsupported,
}

/// An interface's connection state (`WifiState`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub conn: Conn,
    pub ssid: String,
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    pub id: String,
    pub why: Why,
}

impl State {
    /// A join is under way: the page keeps refreshing quickly and holds the
    /// buttons that would start another.
    pub fn busy(&self) -> bool {
        matches!(
            self.conn,
            Conn::Scanning | Conn::Authenticating | Conn::Associating | Conn::Handshake
        )
    }
}

/// The signal as 0 to 4 bars, from the dBm the radio measured.
pub fn bars(rssi_dbm: i32) -> u8 {
    match rssi_dbm {
        r if r >= -55 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        r if r >= -85 => 1,
        _ => 0,
    }
}

/// The signal for a table cell: ASCII bars and the figure, so it renders in
/// any font (`[###.] -52 dBm`).
pub fn signal(rssi_dbm: i32) -> String {
    let lit = usize::from(bars(rssi_dbm));
    format!("[{}{}] {rssi_dbm} dBm", "#".repeat(lit), ".".repeat(4 - lit))
}

/// Side of the tray icon mask, in pixels (the shell takes up to 64).
pub const MASK_SIDE: u32 = 32;

/// The signal-bars tray icon as an RGBA mask (only alpha is used, the shell
/// tints it with the bar's ink so it follows the theme): four bars of rising
/// height, the first `lit` of them solid and the rest faint.
pub fn signal_mask(lit: u8) -> Vec<u8> {
    const SIDE: usize = MASK_SIDE as usize;
    const BASELINE: usize = 28;
    const WIDTH: usize = 5;
    const PITCH: usize = 7;
    const LEFT: usize = 2;
    const FAINT: u8 = 70;
    let mut data = vec![0u8; SIDE * SIDE * 4];
    for bar in 0..4usize {
        let height = 8 + bar * 6;
        let alpha = if bar < usize::from(lit) { 255 } else { FAINT };
        for y in BASELINE - height..BASELINE {
            for x in LEFT + bar * PITCH..LEFT + bar * PITCH + WIDTH {
                let at = (y * SIDE + x) * 4;
                data[at..at + 4].copy_from_slice(&[255, 255, 255, alpha]);
            }
        }
    }
    data
}

/// The band a frequency (MHz) is in.
pub fn band(freq_mhz: u32) -> &'static str {
    match freq_mhz {
        2400..=2500 => "2.4 GHz",
        4900..=5900 => "5 GHz",
        5925..=7125 => "6 GHz",
        _ => "?",
    }
}

/// The 802.11 channel number of a frequency, when it is a standard one.
pub fn channel(freq_mhz: u32) -> Option<u32> {
    match freq_mhz {
        2484 => Some(14),
        2412..=2472 => Some((freq_mhz - 2407) / 5),
        5000..=5900 => Some((freq_mhz - 5000) / 5),
        5955..=7115 => Some((freq_mhz - 5950) / 5),
        _ => None,
    }
}

/// One row of the networks table: name, signal, security, state.
pub fn row(network: &Network) -> Vec<String> {
    let state = if network.connected {
        "Connected"
    } else if network.known() {
        "Saved"
    } else {
        ""
    };
    vec![
        network.display_name().to_string(),
        signal(network.rssi_dbm),
        network.security.text().to_string(),
        state.to_string(),
    ]
}

/// Why an attempt failed, in words the person can act on.
pub fn why_text(why: Why) -> &'static str {
    match why {
        Why::None => "",
        Why::NotFound => "the network was not heard",
        Why::Rejected => "the access point refused the connection",
        Why::BadPassphrase => "probably a wrong passphrase",
        Why::Timeout => "the access point did not answer",
        Why::Deauthenticated => "the access point ended the connection",
        Why::BeaconLoss => "the access point went out of range",
        Why::PolicyMismatch => "the access point's security settings changed",
        Why::Radio => "the Wi-Fi hardware reported an error",
        Why::Unsupported => "this network uses security LazyOS cannot join yet",
    }
}

/// The interface's state as one line.
pub fn state_line(ifname: &str, state: &State) -> String {
    let name = if state.ssid.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", state.ssid)
    };
    match state.conn {
        Conn::Disabled => format!("{ifname}: no Wi-Fi radio is running"),
        Conn::Idle => format!("{ifname}: not connected"),
        Conn::Scanning => format!("{ifname}: looking for networks{name}..."),
        Conn::Authenticating | Conn::Associating => format!("{ifname}: joining{name}..."),
        Conn::Handshake => format!("{ifname}: securing the connection{name}..."),
        Conn::Connected => {
            let channel = channel(state.freq_mhz)
                .map(|c| format!(", channel {c}"))
                .unwrap_or_default();
            format!(
                "{ifname}: connected to{name} on {}{channel}, {}",
                band(state.freq_mhz),
                signal(state.rssi_dbm)
            )
        }
        Conn::Failed => format!(
            "{ifname}: could not join{name}: {}",
            why_text(state.why)
        ),
    }
}

/// The SSID of a hidden network, checked: 1 to 32 bytes, no control
/// characters. The text is not trimmed: a name may start or end in a space.
pub fn check_ssid(ssid: &str) -> Result<(), String> {
    if ssid.is_empty() {
        return Err(String::from("Type the network's name."));
    }
    if ssid.len() > MAX_SSID {
        return Err(format!("A network name is at most {MAX_SSID} bytes."));
    }
    if ssid.chars().any(char::is_control) {
        return Err(String::from("A network name cannot hold control characters."));
    }
    Ok(())
}

/// A WPA2 passphrase: 8 to 63 octets without control characters, or exactly
/// 64 hex digits (a raw PSK).
pub fn check_passphrase(text: &str) -> Result<(), String> {
    if text.chars().any(char::is_control) {
        return Err(String::from("The passphrase cannot hold control characters."));
    }
    let len = text.len();
    if len == PSK_HEX {
        return if text.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(())
        } else {
            Err(format!(
                "A {PSK_HEX}-character passphrase must be hexadecimal; \
                 shorter ones are at most {MAX_PASSPHRASE}."
            ))
        };
    }
    if (MIN_PASSPHRASE..=MAX_PASSPHRASE).contains(&len) {
        Ok(())
    } else {
        Err(format!(
            "A WPA2 passphrase is {MIN_PASSPHRASE} to {MAX_PASSPHRASE} characters."
        ))
    }
}

/// `text` without control characters and at most `max` bytes (cut on a
/// character boundary), so a peer's name cannot break a label.
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, security: Security, rssi: i32, id: &str, connected: bool) -> Network {
        Network {
            ssid: ssid.into(),
            security,
            rssi_dbm: rssi,
            freq_mhz: 5500,
            bss_count: 1,
            id: id.into(),
            connected,
        }
    }

    #[test]
    fn bars_follow_the_usual_thresholds() {
        assert_eq!(bars(-30), 4);
        assert_eq!(bars(-55), 4);
        assert_eq!(bars(-56), 3);
        assert_eq!(bars(-70), 2);
        assert_eq!(bars(-80), 1);
        assert_eq!(bars(-95), 0);
        assert_eq!(signal(-52), "[####] -52 dBm");
        assert_eq!(signal(-90), "[....] -90 dBm");
    }

    #[test]
    fn channels_and_bands() {
        assert_eq!(channel(2412), Some(1));
        assert_eq!(channel(2462), Some(11));
        assert_eq!(channel(2484), Some(14));
        assert_eq!(channel(5500), Some(100));
        assert_eq!(channel(5180), Some(36));
        assert_eq!(channel(5955), Some(1));
        assert_eq!(channel(1234), None);
        assert_eq!(band(2437), "2.4 GHz");
        assert_eq!(band(5500), "5 GHz");
        assert_eq!(band(6115), "6 GHz");
        assert_eq!(band(0), "?");
    }

    #[test]
    fn rows_say_what_is_known() {
        let saved = net("Home", Security::Wpa2Psk, -60, "w0123456789ab", false);
        assert_eq!(
            row(&saved),
            ["Home", "[###.] -60 dBm", "WPA2", "Saved"]
        );
        let up = net("Home", Security::Wpa2Psk, -40, "w0123456789ab", true);
        assert_eq!(row(&up)[3], "Connected");
        let cafe = net("Cafe", Security::Open, -80, "", false);
        assert_eq!(row(&cafe), ["Cafe", "[#...] -80 dBm", "Open", ""]);
        let hidden = net("", Security::Wpa2Psk, -70, "", false);
        assert_eq!(row(&hidden)[0], "(hidden network)");
    }

    #[test]
    fn state_lines() {
        let mut state = State::default();
        assert_eq!(state_line("wlan0", &state), "wlan0: no Wi-Fi radio is running");
        state.conn = Conn::Idle;
        assert_eq!(state_line("wlan0", &state), "wlan0: not connected");
        state.conn = Conn::Connected;
        state.ssid = "Home".into();
        state.freq_mhz = 5500;
        state.rssi_dbm = -52;
        assert_eq!(
            state_line("wlan0", &state),
            "wlan0: connected to \"Home\" on 5 GHz, channel 100, [####] -52 dBm"
        );
        state.conn = Conn::Failed;
        state.why = Why::BadPassphrase;
        assert_eq!(
            state_line("wlan0", &state),
            "wlan0: could not join \"Home\": probably a wrong passphrase"
        );
        assert!(!state.busy());
        state.conn = Conn::Handshake;
        assert!(state.busy());
    }

    #[test]
    fn passphrase_rules() {
        assert!(check_passphrase("12345678").is_ok());
        assert!(check_passphrase(&"x".repeat(63)).is_ok());
        assert!(check_passphrase("short").is_err());
        assert!(check_passphrase(&"x".repeat(64)).is_err(), "64 must be hex");
        assert!(check_passphrase(&"a".repeat(64)).is_ok());
        assert!(check_passphrase(&"x".repeat(65)).is_err());
        assert!(check_passphrase("pass\nword1").is_err());
    }

    #[test]
    fn ssid_rules() {
        assert!(check_ssid("Home").is_ok());
        assert!(check_ssid("").is_err());
        assert!(check_ssid(&"s".repeat(33)).is_err());
        assert!(check_ssid("a\u{7}b").is_err());
    }

    #[test]
    fn hostile_names_are_cleaned() {
        assert_eq!(clean("Ho\u{1b}[31mme", 32), "Ho[31mme");
        assert_eq!(clean("abcdef", 3), "abc");
        // Never cut inside a character.
        assert_eq!(clean("aé", 2), "a");
    }

    #[test]
    fn mask_lights_the_asked_bars() {
        let side = MASK_SIDE as usize;
        let mask = signal_mask(2);
        assert_eq!(mask.len(), side * side * 4);
        // The alpha of a pixel in the middle of bar `n`, one row above the
        // baseline.
        let alpha = |bar: usize| mask[(27 * side + 2 + bar * 7 + 2) * 4 + 3];
        assert_eq!([alpha(0), alpha(1), alpha(2), alpha(3)], [255, 255, 70, 70]);
        assert!(signal_mask(0).chunks(4).all(|px| px[3] == 0 || px[3] == 70));
        assert_eq!(signal_mask(4)[(27 * side + 23 + 2) * 4 + 3], 255);
        // The tallest bar is taller than the shortest.
        let column = |x: usize| (0..side).filter(|y| mask[(y * side + x) * 4 + 3] > 0).count();
        assert!(column(2 + 3 * 7 + 2) > column(2 + 2));
    }

    #[test]
    fn security_flags() {
        assert!(Security::Wpa2Psk.needs_passphrase());
        assert!(!Security::Open.needs_passphrase());
        assert!(!Security::Unsupported.joinable());
        assert_eq!(Security::Unsupported.text(), "Not supported");
    }
}

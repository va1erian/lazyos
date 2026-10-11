//! SSIDs as text for clients.
//!
//! An SSID is up to 32 octets anyone in radio range chose: not necessarily
//! UTF-8, and free to carry control characters, terminal escapes or the
//! bidirectional overrides that make text read differently from what it is.
//! [`display`] is the only way a heard name reaches a client
//! (`os.lazy.net.wifi.v1` `Network.ssid`, `WifiState.ssid`, serial lines).

use alloc::string::String;

/// Longest SSID in octets (IEEE 802.11).
pub const MAX_SSID: usize = 32;

/// Characters that change how surrounding text is shown without being seen:
/// the bidirectional embeddings, overrides and isolates, zero-width joiners
/// and marks, and the byte order mark.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// `ssid` as display text: invalid UTF-8 replaced with U+FFFD, control and
/// invisible formatting characters dropped, and the result cut on a character
/// boundary to at most [`MAX_SSID`] octets. An all-NUL (hidden) name is empty.
pub fn display(ssid: &[u8]) -> String {
    let mut out = String::new();
    for c in String::from_utf8_lossy(ssid).chars() {
        if c.is_control() || invisible(c) {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_SSID {
            break;
        }
        out.push(c);
    }
    out
}

/// Whether `ssid` is one a profile may name: 1 to 32 octets.
pub fn valid(ssid: &[u8]) -> bool {
    (1..=MAX_SSID).contains(&ssid.len())
}

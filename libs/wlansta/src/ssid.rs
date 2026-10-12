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

/// Characters that change how surrounding text is shown, or what a line is,
/// without being seen: the bidirectional marks, embeddings, overrides and
/// isolates (U+061C, U+200E-200F, U+202A-202E, U+2066-2069), the line and
/// paragraph separators (U+2028-2029), the zero-width space, word joiner and
/// invisible operators (U+200B, U+2060-2064), the deprecated format
/// characters (U+206A-206F), the Mongolian vowel separator (U+180E), the
/// byte order mark, the interlinear annotation characters (U+FFF9-FFFB) and
/// the tag characters (U+E0000-E007F). The zero-width joiner and non-joiner
/// (U+200C-200D) stay: emoji sequences and several scripts need them.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// `ssid` as display text: invalid UTF-8 replaced with U+FFFD, control and
/// invisible formatting characters dropped, and the result cut on a character
/// boundary to at most [`MAX_SSID`] octets. An all-NUL (hidden) name is empty.
/// Two different SSIDs may display alike (`home` and `home` + U+200B); a
/// front end names a network by its profile id, and `wlanmd` matches a typed
/// name against the raw SSIDs first ([`matches`]).
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

/// How a typed name names a heard SSID: the raw octets themselves, or only
/// its [`display`] form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    Exact,
    Display,
}

/// Whether `typed` names the heard SSID `raw`, and how.
pub fn matches(typed: &[u8], raw: &[u8]) -> Option<Match> {
    if typed == raw {
        Some(Match::Exact)
    } else if !typed.is_empty() && display(raw).as_bytes() == typed {
        Some(Match::Display)
    } else {
        None
    }
}

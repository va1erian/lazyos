//! `net.wifi.add` and `net.wifi.forget`: a Wi-Fi network for every user.
//!
//! A user saves their own network with `wlanmd` directly (`AddNetwork`,
//! scope `User`). A network for **all users** (also before anyone logs in)
//! is the machine's, so an administrator approves it: `elevd` keeps the
//! passphrase in `keyd` as a `system` secret, then asks `wlanmd` for the
//! profile of scope `System` under the same id (`wlanmd` takes that scope
//! from `elevd` alone, `wlansta::policy::add_owner`). The id is `elevd`'s
//! to choose, so it is made there, never taken from the asker. The
//! passphrase is checked here with the rule `keyd` derives the PMK by, and
//! the prompt never shows it ([`Operation::summary`]).
//!
//! `net.wifi.forget` is the way back: it removes the profile and then the
//! `system` secret of that id.

use alloc::format;
use alloc::string::{String, ToString};

use crate::text::{self, plain};
use crate::values::wifi_passphrase;
use crate::Operation;

/// Longest SSID, in octets (IEEE 802.11).
pub const MAX_SSID: usize = 32;
/// Longest SSID the prompt shows (quotes and escapes aside).
const SSID_SHOWN: usize = 48;

/// An SSID the profile may name: 1 to 32 octets with nothing that can make
/// the prompt read other than it is.
pub(crate) fn ssid(text: &str) -> Result<String, &'static str> {
    if (1..=MAX_SSID).contains(&text.len()) && plain(text) {
        Ok(text.to_string())
    } else {
        Err("a network name is 1 to 32 bytes of printable text")
    }
}

/// A profile id as `wlanmd` makes them: `w` and 12 lowercase hex digits,
/// also a safe `keyd` secret name and `confd` path segment.
pub fn valid_id(id: &str) -> bool {
    id.len() == 13
        && id.starts_with('w')
        && id[1..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `net.wifi.add`: SSID, `open` or `wpa2`, hidden (`1`/`0`), passphrase
/// (empty for `open`, required for `wpa2`).
pub(crate) fn parse_add(args: &[String]) -> Result<Operation, &'static str> {
    let [name, security, hidden, passphrase] = args else {
        return Err("wrong number of arguments");
    };
    let wpa2 = match security.as_str() {
        "open" => false,
        "wpa2" => true,
        _ => return Err("the security is open or wpa2"),
    };
    let hidden = match hidden.as_str() {
        "1" => true,
        "0" => false,
        _ => return Err("hidden is 1 or 0"),
    };
    let passphrase = match (wpa2, passphrase.is_empty()) {
        (true, _) => wifi_passphrase(passphrase)?,
        (false, true) => String::new(),
        (false, false) => return Err("an open network has no password"),
    };
    Ok(Operation::WifiAdd {
        ssid: ssid(name)?,
        wpa2,
        hidden,
        passphrase,
    })
}

/// `net.wifi.forget`: a profile id.
pub(crate) fn parse_forget(args: &[String]) -> Result<Operation, &'static str> {
    let [id] = args else {
        return Err("wrong number of arguments");
    };
    if valid_id(id) {
        Ok(Operation::WifiForget { id: id.clone() })
    } else {
        Err("not a Wi-Fi network id")
    }
}

/// The prompt text of `net.wifi.add`: never the passphrase.
pub(crate) fn add_summary(ssid: &str, wpa2: bool, hidden: bool) -> String {
    format!(
        "Save the {}Wi-Fi network {} ({}) for all users, before login too",
        if hidden { "hidden " } else { "" },
        text::quoted(ssid, SSID_SHOWN),
        if wpa2 { "WPA2 password" } else { "open" },
    )
}

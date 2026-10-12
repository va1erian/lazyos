//! The officially assigned ISO 3166-1 alpha-2 country codes (249) with their
//! English short names, and the world domain `00`.
//!
//! One list serves every side of the Wi-Fi regulatory country
//! (`sys/net/wifi/country`): the soft-MAC refuses a code that is not on it
//! (`wifimac::regdom`), `elevd` validates the `net.wifi.country` operation
//! against it, and Settings offers it. A made-up code (`ZZ`) is not a
//! country: it must not unlock what a named country may do (active probing
//! on 5 GHz, channels 12 and 13).
//!
//! Names are plain ASCII, so a prompt or a label shows them as they are.

#![no_std]
#![forbid(unsafe_code)]

mod table;

pub use table::COUNTRIES;

/// The world domain: no country, the most restrictive rules (`wifimac`).
pub const WORLD: &str = "00";
/// The world domain's display name.
pub const WORLD_NAME: &str = "World";

/// Whether `code` is an assigned ISO 3166-1 alpha-2 code (`00` is not one).
pub fn is_country(code: &[u8; 2]) -> bool {
    index(code).is_some()
}

/// The English short name of the country `code` (`None` for `00` and for
/// anything that is not an assigned code).
pub fn name(code: &[u8; 2]) -> Option<&'static str> {
    index(code).map(|at| COUNTRIES[at].1)
}

/// The regulatory code `text` names, upper-cased: `00` or an assigned
/// country; anything else (a wrong length, `ZZ`, a non-ASCII byte) is refused.
pub fn parse(text: &str) -> Option<[u8; 2]> {
    let bytes: [u8; 2] = text.as_bytes().try_into().ok()?;
    let code = bytes.map(|b| b.to_ascii_uppercase());
    (code == *b"00" || is_country(&code)).then_some(code)
}

/// The display name of a regulatory code: the country's, or "World" for `00`.
pub fn display_name(code: &[u8; 2]) -> Option<&'static str> {
    if code == b"00" {
        Some(WORLD_NAME)
    } else {
        name(code)
    }
}

fn index(code: &[u8; 2]) -> Option<usize> {
    COUNTRIES
        .binary_search_by(|(known, _)| known.as_bytes().cmp(code.as_slice()))
        .ok()
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

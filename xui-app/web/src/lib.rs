//! LazyWeb's library half: the parts with no window, tested on the host.
//!
//! - [`fetch`]: HTTP and HTTPS for NetSurf (ureq, rustls, `nettls-crypto`).
//! - [`address`]: what the address bar turns typed text into.
//! - [`history`]: the Back/Forward list.

pub mod address;
pub mod fetch;
pub mod history;

/// `text` fit for one serial marker line: control characters (a title can
/// hold newlines) become spaces.
pub fn marker_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

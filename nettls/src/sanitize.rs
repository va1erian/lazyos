//! Server-controlled text bound for the terminal (headers, certificate
//! subjects, status reasons) is reduced to printable ASCII, so a hostile
//! server cannot send escape sequences that rewrite the screen, retitle the
//! window or fake a later line (the rule `ftp` follows).

/// `bytes` with every byte outside printable ASCII replaced by `?`; a tab
/// becomes a space.
pub fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            b'\t' => ' ',
            0x20..=0x7e => b as char,
            _ => '?',
        })
        .collect()
}

/// [`printable`] for text that is already a `str`.
pub fn printable_str(text: &str) -> String {
    printable(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_escapes() {
        assert_eq!(printable(b"ok\x1b[2J\r\nx\ty"), "ok?[2J??x y");
        assert_eq!(printable("caf\u{e9}".as_bytes()), "caf??");
        assert_eq!(printable_str("plain text 123"), "plain text 123");
        assert_eq!(printable(&[0x7f, 0x00]), "??");
    }
}

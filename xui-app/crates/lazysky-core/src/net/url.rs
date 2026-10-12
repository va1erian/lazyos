//! Query strings for XRPC GETs.

/// Percent-encodes `s` for a query value: everything but RFC 3986's
/// unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `base` with `?k=v&k=v` appended (values encoded; empty values kept).
pub fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let mut url = base.to_string();
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_and_non_ascii_bytes_are_encoded() {
        assert_eq!(
            encode("at://did:plc:a/app.bsky.feed.post/1"),
            "at%3A%2F%2Fdid%3Aplc%3Aa%2Fapp.bsky.feed.post%2F1"
        );
        assert_eq!(encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(encode("Az09-._~"), "Az09-._~");
    }

    #[test]
    fn queries_join_with_question_mark_and_ampersand() {
        let url = with_query(
            "https://h.test/xrpc/x",
            &[("limit", "30"), ("cursor", "a/b")],
        );
        assert_eq!(url, "https://h.test/xrpc/x?limit=30&cursor=a%2Fb");
        assert_eq!(with_query("https://h.test", &[]), "https://h.test");
    }
}

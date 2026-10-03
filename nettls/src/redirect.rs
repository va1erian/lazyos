//! URL rules: which URLs a run accepts, and where a redirect may lead.
//!
//! - Only `https` and `http`. A URL without a scheme is taken as `https`
//!   (curl and wget assume `http`; on this system the safe guess wins).
//! - No credentials in the URL: they would sit in argv, visible to every
//!   process, and be sent on every redirect.
//! - A redirect from `https` to `http` is refused: following it would hand
//!   the rest of the exchange to anyone on the path, silently.
//! - Each redirect counts against the limit; exceeding it is a failure.
//! - Credential headers the user gave (`Authorization`, `Cookie`, ...) go
//!   only to the origin they named, as curl does without
//!   `--location-trusted`: a redirect must not hand them to another host.

use url::Url;

use crate::report::Failure;

/// Parse the URL a user typed.
pub fn parse_user_url(text: &str) -> Result<Url, Failure> {
    let text = text.trim();
    // Only a leading `scheme://` counts: `a.example/?to=https://b` has none.
    let has_scheme = text.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    let with_scheme = if has_scheme {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let url = Url::parse(&with_scheme).map_err(|e| Failure::BadUrl(format!("{text}: {e}")))?;
    check_url(&url)?;
    Ok(url)
}

/// The rules every URL we connect to must satisfy.
pub fn check_url(url: &Url) -> Result<(), Failure> {
    match url.scheme() {
        "https" | "http" => {}
        other => return Err(Failure::UnsupportedScheme(other.to_string())),
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(Failure::BadUrl(format!("{url}: no host")));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Failure::BadUrl(
            "credentials in the URL are not supported (they would be visible to every \
             process and resent on redirects)"
                .into(),
        ));
    }
    Ok(())
}

/// True for the statuses that carry a `Location` to follow.
pub fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Where a redirect from `current` with `location` leads, after the rules.
/// `followed` is how many redirects were already followed; `limit` is the
/// most allowed.
pub fn next_hop(
    current: &Url,
    location: Option<&[u8]>,
    followed: u32,
    limit: u32,
) -> Result<Url, Failure> {
    if followed >= limit {
        return Err(Failure::TooManyRedirects(limit));
    }
    let raw = location.ok_or_else(|| Failure::BadRedirect("no Location header".into()))?;
    let text = std::str::from_utf8(raw)
        .map_err(|_| Failure::BadRedirect("Location is not valid UTF-8".into()))?;
    let mut next = current
        .join(text.trim())
        .map_err(|e| Failure::BadRedirect(format!("Location is not a URL: {e}")))?;
    // A fragment is never sent; the new one (or the old, per RFC 9110
    // 10.2.2) is meaningless to a downloader.
    next.set_fragment(None);
    if current.scheme() == "https" && next.scheme() == "http" {
        return Err(Failure::Downgrade(crate::sanitize::printable_str(
            next.as_str(),
        )));
    }
    check_url(&next)?;
    Ok(next)
}

/// True when `a` and `b` share scheme, host and port.
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Headers that carry credentials, sent only to the origin the user named.
pub fn is_credential_header(name: &str) -> bool {
    ["authorization", "proxy-authorization", "cookie"]
        .iter()
        .any(|c| name.eq_ignore_ascii_case(c))
}

/// The URL as sent on the wire: without the fragment.
pub fn request_target(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn user_urls() {
        assert_eq!(
            parse_user_url("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            parse_user_url("http://a/b?c").unwrap().as_str(),
            "http://a/b?c"
        );
        assert!(matches!(
            parse_user_url("ftp://a/"),
            Err(Failure::UnsupportedScheme(_))
        ));
        assert_eq!(
            parse_user_url("example.com/a?u=http://b").unwrap().as_str(),
            "https://example.com/a?u=http://b"
        );
        assert!(parse_user_url("file:///etc/passwd").is_err());
        assert!(matches!(
            parse_user_url("https://u:p@a/"),
            Err(Failure::BadUrl(_))
        ));
        assert!(matches!(
            parse_user_url("https://"),
            Err(Failure::BadUrl(_))
        ));
        assert!(matches!(
            parse_user_url("https://exa mple.com/"),
            Err(Failure::BadUrl(_))
        ));
    }

    #[test]
    fn relative_and_absolute_locations() {
        let base = url("https://a.example/dir/page?q=1");
        let hop = |loc: &str| next_hop(&base, Some(loc.as_bytes()), 0, 5);
        assert_eq!(hop("/top").unwrap().as_str(), "https://a.example/top");
        assert_eq!(
            hop("other").unwrap().as_str(),
            "https://a.example/dir/other"
        );
        assert_eq!(hop("../up").unwrap().as_str(), "https://a.example/up");
        assert_eq!(
            hop("//b.example/x").unwrap().as_str(),
            "https://b.example/x"
        );
        assert_eq!(
            hop("https://c.example/#frag").unwrap().as_str(),
            "https://c.example/"
        );
    }

    #[test]
    fn downgrade_is_refused_upgrade_allowed() {
        let secure = url("https://a.example/");
        let err = next_hop(&secure, Some(b"http://a.example/"), 0, 5).unwrap_err();
        assert!(matches!(err, Failure::Downgrade(_)));
        assert_eq!(err.curl_code(), 1);
        let plain = url("http://a.example/");
        assert_eq!(
            next_hop(&plain, Some(b"https://a.example/"), 0, 5)
                .unwrap()
                .as_str(),
            "https://a.example/"
        );
    }

    #[test]
    fn limits_and_bad_locations() {
        let base = url("https://a.example/");
        assert_eq!(
            next_hop(&base, Some(b"/x"), 3, 3),
            Err(Failure::TooManyRedirects(3))
        );
        assert_eq!(
            next_hop(&base, Some(b"/x"), 0, 0),
            Err(Failure::TooManyRedirects(0))
        );
        assert!(matches!(
            next_hop(&base, None, 0, 5),
            Err(Failure::BadRedirect(_))
        ));
        assert!(matches!(
            next_hop(&base, Some(b"\xff\xfe"), 0, 5),
            Err(Failure::BadRedirect(_))
        ));
        assert!(matches!(
            next_hop(&base, Some(b"ftp://a/"), 0, 5),
            Err(Failure::UnsupportedScheme(_))
        ));
        assert!(matches!(
            next_hop(&base, Some(b"https://user:pw@b/"), 0, 5),
            Err(Failure::BadUrl(_))
        ));
        assert!(is_redirect(302) && is_redirect(308) && !is_redirect(304) && !is_redirect(200));
    }

    #[test]
    fn credentials_stay_with_their_origin() {
        let start = url("https://a.example/x");
        assert!(same_origin(&start, &url("https://a.example:443/y")));
        assert!(!same_origin(&start, &url("https://evil.example/")));
        assert!(!same_origin(&start, &url("https://a.example:8443/")));
        assert!(!same_origin(&url("http://a.example/"), &start));
        assert!(is_credential_header("Authorization") && is_credential_header("COOKIE"));
        assert!(is_credential_header("proxy-authorization"));
        assert!(!is_credential_header("Accept") && !is_credential_header("X-Cookie"));
    }
}

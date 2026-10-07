//! The cookie jar: what `Set-Cookie` responses leave and `Cookie` requests
//! carry (RFC 6265, the part a browser without scripts needs).
//!
//! NetSurf kept a jar of its own; `xui-blitz` has none, so the fetcher does
//! (issue #649): logins and sites that set a session cookie before a redirect
//! need it. The jar lives in memory for the process (a session): nothing is
//! written to disk. Every hop of a redirect passes through the fetcher, so a
//! cookie set by a 302 reaches the next request.
//!
//! The fetcher cannot tell a subresource from the page that loaded it, so a
//! third-party image carries its own site's cookies like any other request
//! (no `SameSite` handling, and no cookie is ever readable by the page: there
//! are no scripts). Limits keep a hostile site from filling memory.

use std::time::{SystemTime, UNIX_EPOCH};

/// The most cookies kept; the oldest go first.
const MAX_COOKIES: usize = 3000;
/// The most cookies kept for one host name.
const MAX_PER_DOMAIN: usize = 50;
/// The longest `name=value` (and each attribute value) accepted.
const MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    /// Lowercase, no leading dot.
    domain: String,
    /// Sent only to exactly `domain` (no `Domain` attribute).
    host_only: bool,
    path: String,
    secure: bool,
    /// Seconds since the epoch; `None` for a session cookie.
    expires: Option<u64>,
    /// Creation order, for the `Cookie` header and for eviction.
    seq: u64,
}

/// The cookies the browser holds.
#[derive(Debug, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    next_seq: u64,
}

/// Seconds since the epoch now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Jar {
    /// Stores the cookie a `Set-Cookie` header of `url`'s response sets.
    /// A malformed or refused one is ignored, as browsers do.
    pub fn store(&mut self, url: &str, header: &str, now: u64) {
        let Some(target) = Target::of(url) else {
            return;
        };
        let Some(cookie) = self.parse(&target, header, now) else {
            return;
        };
        self.cookies
            .retain(|c| !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path));
        // An expired cookie only deletes what it replaces.
        if cookie.expires.is_some_and(|at| at <= now) {
            return;
        }
        self.cookies.push(cookie);
        self.trim(now);
    }

    /// The `Cookie` header value for a request to `url`, if any cookie applies.
    pub fn header(&self, url: &str, now: u64) -> Option<String> {
        let target = Target::of(url)?;
        let mut sent: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.expires.is_none_or(|at| at > now))
            .filter(|c| !c.secure || target.https)
            .filter(|c| domain_matches(&target.host, &c.domain, c.host_only))
            .filter(|c| path_matches(&target.path, &c.path))
            .collect();
        if sent.is_empty() {
            return None;
        }
        // Longer paths first, then the older cookie (RFC 6265 §5.4).
        sent.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.seq.cmp(&b.seq)));
        let pairs: Vec<String> = sent
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        Some(pairs.join("; "))
    }

    /// How many cookies are held.
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// Whether the jar is empty.
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    fn parse(&mut self, target: &Target, header: &str, now: u64) -> Option<Cookie> {
        let mut parts = header.split(';');
        let (name, value) = parts.next()?.split_once('=')?;
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || name.len() + value.len() > MAX_BYTES {
            return None;
        }
        let mut domain = None;
        let mut path = None;
        let mut secure = false;
        let mut expires = None;
        let mut max_age = None;
        for attribute in parts {
            let (key, val) = attribute
                .split_once('=')
                .map_or((attribute, ""), |(k, v)| (k, v));
            let (key, val) = (key.trim().to_ascii_lowercase(), val.trim());
            if val.len() > MAX_BYTES {
                continue;
            }
            match key.as_str() {
                "domain" => domain = Some(val.trim_start_matches('.').to_ascii_lowercase()),
                "path" => path = Some(val.to_string()),
                "secure" => secure = true,
                "expires" => expires = parse_date(val),
                "max-age" => max_age = val.parse::<i64>().ok(),
                _ => {}
            }
        }
        // A secure cookie only from a secure origin.
        if secure && !target.https {
            return None;
        }
        let (domain, host_only) = match domain.filter(|d| !d.is_empty()) {
            None => (target.host.clone(), true),
            Some(domain) => {
                // The host must be inside the domain it names.
                if !domain_matches(&target.host, &domain, false)
                    || is_ip(&target.host) && domain != target.host
                {
                    return None;
                }
                if is_ip(&domain) || psl::domain_str(&domain).is_none() {
                    // An address, or a public suffix ("com", "co.uk",
                    // "github.io"): a cookie for it would reach every site
                    // under it. Only that very host may set it, for itself.
                    if domain != target.host {
                        return None;
                    }
                    (target.host.clone(), true)
                } else {
                    (domain, false)
                }
            }
        };
        let path = match path {
            Some(p) if p.starts_with('/') => p,
            _ => default_path(&target.path),
        };
        // Max-Age wins over Expires.
        let expires = match max_age {
            Some(seconds) if seconds <= 0 => Some(0),
            Some(seconds) => Some(now.saturating_add(seconds as u64)),
            None => expires,
        };
        self.next_seq += 1;
        Some(Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain,
            host_only,
            path,
            secure,
            expires,
            seq: self.next_seq,
        })
    }

    /// Drops expired cookies, then the oldest while a limit is exceeded.
    fn trim(&mut self, now: u64) {
        self.cookies.retain(|c| c.expires.is_none_or(|at| at > now));
        let newest = self.cookies.last().map(|c| c.domain.clone());
        if let Some(domain) = newest {
            while self.cookies.iter().filter(|c| c.domain == domain).count() > MAX_PER_DOMAIN {
                let oldest = self
                    .cookies
                    .iter()
                    .position(|c| c.domain == domain)
                    .expect("the domain has cookies");
                self.cookies.remove(oldest);
            }
        }
        if self.cookies.len() > MAX_COOKIES {
            let excess = self.cookies.len() - MAX_COOKIES;
            self.cookies.drain(..excess);
        }
    }
}

/// The parts of a request URL cookies care about.
struct Target {
    https: bool,
    host: String,
    path: String,
}

impl Target {
    fn of(url: &str) -> Option<Target> {
        let (scheme, rest) = url.split_once("://")?;
        let https = scheme.eq_ignore_ascii_case("https");
        if !https && !scheme.eq_ignore_ascii_case("http") {
            return None;
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        let authority = authority.rsplit('@').next().unwrap_or("");
        let host = match authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => authority.split(':').next().unwrap_or(""),
        };
        if host.is_empty() {
            return None;
        }
        let path = tail.split(['?', '#']).next().unwrap_or("");
        let path = if path.is_empty() { "/" } else { path };
        Some(Target {
            https,
            host: host.to_ascii_lowercase(),
            path: path.to_string(),
        })
    }
}

fn is_ip(host: &str) -> bool {
    host.contains(':') || host.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// RFC 6265 §5.1.3.
fn domain_matches(host: &str, domain: &str, host_only: bool) -> bool {
    if host == domain {
        return true;
    }
    !host_only
        && !is_ip(host)
        && host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

/// RFC 6265 §5.1.4.
fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/'))
}

/// RFC 6265 §5.1.4: the request path up to its last `/`.
fn default_path(request: &str) -> String {
    match request.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => request[..i].to_string(),
    }
}

/// An `Expires` date (RFC 6265 §5.1.1, tolerant of the common layouts) as
/// seconds since the epoch; `None` when it is not one.
fn parse_date(text: &str) -> Option<u64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in text.split(|c: char| !c.is_ascii_alphanumeric() && c != ':') {
        if token.is_empty() {
            continue;
        }
        if time.is_none() && token.contains(':') {
            let mut parts = token.split(':').map(|p| p.parse::<u64>().ok());
            if let (Some(Some(h)), Some(Some(m)), Some(Some(s))) =
                (parts.next(), parts.next(), parts.next())
            {
                time = Some((h, m, s));
                continue;
            }
        }
        if day.is_none() && token.len() <= 2 {
            if let Ok(d) = token.parse::<u64>() {
                day = Some(d);
                continue;
            }
        }
        if month.is_none() && token.len() >= 3 {
            let prefix = token[..3].to_ascii_lowercase();
            const MONTHS: [&str; 12] = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ];
            if let Some(i) = MONTHS.iter().position(|m| *m == prefix) {
                month = Some(i as u64 + 1);
                continue;
            }
        }
        if year.is_none() && token.len() >= 2 && token.len() <= 4 {
            if let Ok(y) = token.parse::<u64>() {
                year = Some(match y {
                    0..=69 => y + 2000,
                    70..=99 => y + 1900,
                    _ => y,
                });
            }
        }
    }
    let ((h, m, s), day, month, year) = (time?, day?, month?, year?);
    if !(1..=31).contains(&day) || year < 1601 || h > 23 || m > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(year as i64, month as i64, day as i64);
    let seconds = days * 86400 + (h * 3600 + m * 60 + s) as i64;
    Some(seconds.max(0) as u64)
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn sent(jar: &Jar, url: &str) -> String {
        jar.header(url, NOW).unwrap_or_default()
    }

    #[test]
    fn a_cookie_comes_back_to_its_host_only() {
        let mut jar = Jar::default();
        jar.store("http://a.example/x", "sid=1", NOW);
        assert_eq!(sent(&jar, "http://a.example/y"), "sid=1");
        assert_eq!(sent(&jar, "http://b.example/"), "");
        assert_eq!(sent(&jar, "http://www.a.example/"), "");
    }

    #[test]
    fn a_domain_attribute_covers_subdomains() {
        let mut jar = Jar::default();
        jar.store("https://www.example.com/", "k=v; Domain=.example.com", NOW);
        assert_eq!(sent(&jar, "https://api.example.com/"), "k=v");
        assert_eq!(sent(&jar, "https://example.com/"), "k=v");
        assert_eq!(sent(&jar, "https://notexample.com/"), "");
    }

    #[test]
    fn a_foreign_or_bare_domain_is_refused() {
        let mut jar = Jar::default();
        jar.store("http://a.example.com/", "k=v; Domain=other.com", NOW);
        jar.store("http://a.example.com/", "k=v; Domain=com", NOW);
        assert!(jar.is_empty());
    }

    #[test]
    fn a_public_suffix_is_never_a_cookie_domain() {
        let mut jar = Jar::default();
        for (url, domain) in [
            ("http://a.example.co.uk/", "co.uk"),
            ("https://user.github.io/", "github.io"),
            ("https://a.example.com/", "com"),
        ] {
            jar.store(url, &format!("k=v; Domain={domain}"), NOW);
        }
        assert!(jar.is_empty());
        // A real registrable domain under a multi-label suffix is fine.
        jar.store("http://www.example.co.uk/", "k=v; Domain=example.co.uk", NOW);
        assert_eq!(sent(&jar, "http://api.example.co.uk/"), "k=v");
        // A host that is itself a suffix keeps the cookie to itself.
        jar.store("http://localhost/", "l=1; Domain=localhost", NOW);
        assert_eq!(sent(&jar, "http://localhost/"), "l=1");
        assert_eq!(sent(&jar, "http://x.localhost/"), "");
    }

    #[test]
    fn paths_match_on_a_boundary() {
        let mut jar = Jar::default();
        jar.store("http://h.example/", "a=1; Path=/app", NOW);
        assert_eq!(sent(&jar, "http://h.example/app"), "a=1");
        assert_eq!(sent(&jar, "http://h.example/app/x"), "a=1");
        assert_eq!(sent(&jar, "http://h.example/apple"), "");
    }

    #[test]
    fn the_default_path_is_the_directory() {
        let mut jar = Jar::default();
        jar.store("http://h.example/dir/page", "a=1", NOW);
        assert_eq!(sent(&jar, "http://h.example/dir/other"), "a=1");
        assert_eq!(sent(&jar, "http://h.example/"), "");
        assert_eq!(default_path("/page"), "/");
    }

    #[test]
    fn secure_cookies_stay_on_https() {
        let mut jar = Jar::default();
        jar.store("http://h.example/", "s=1; Secure", NOW);
        assert!(jar.is_empty());
        jar.store("https://h.example/", "s=1; Secure", NOW);
        assert_eq!(sent(&jar, "https://h.example/"), "s=1");
        assert_eq!(sent(&jar, "http://h.example/"), "");
    }

    #[test]
    fn a_new_value_replaces_the_old_and_max_age_zero_deletes() {
        let mut jar = Jar::default();
        jar.store("http://h.example/", "a=1", NOW);
        jar.store("http://h.example/", "a=2", NOW);
        assert_eq!(sent(&jar, "http://h.example/"), "a=2");
        jar.store("http://h.example/", "a=x; Max-Age=0", NOW);
        assert!(jar.is_empty());
    }

    #[test]
    fn expiry_by_max_age_and_by_date() {
        let mut jar = Jar::default();
        jar.store("http://h.example/", "a=1; Max-Age=10", NOW);
        assert_eq!(jar.header("http://h.example/", NOW + 5).as_deref(), Some("a=1"));
        assert_eq!(jar.header("http://h.example/", NOW + 11), None);
        jar.store(
            "http://h.example/",
            "b=1; Expires=Thu, 01 Jan 1970 00:00:01 GMT",
            NOW,
        );
        assert_eq!(sent(&jar, "http://h.example/"), "a=1");
    }

    #[test]
    fn dates_parse_in_the_common_layouts() {
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_date("Fri, 14 Nov 2023 22:13:20 GMT"), Some(NOW));
        assert_eq!(parse_date("Friday, 14-Nov-23 22:13:20 GMT"), Some(NOW));
        assert_eq!(parse_date("Fri Nov 14 22:13:20 2023"), Some(NOW));
        assert_eq!(parse_date("soon"), None);
    }

    #[test]
    fn longer_paths_and_older_cookies_come_first() {
        let mut jar = Jar::default();
        jar.store("http://h.example/", "a=1", NOW);
        jar.store("http://h.example/", "b=2; Path=/x", NOW);
        jar.store("http://h.example/", "c=3", NOW);
        assert_eq!(sent(&jar, "http://h.example/x/y"), "b=2; a=1; c=3");
    }

    #[test]
    fn a_hostile_site_cannot_fill_the_jar() {
        let mut jar = Jar::default();
        for i in 0..200 {
            jar.store("http://h.example/", &format!("k{i}=v"), NOW);
        }
        assert_eq!(jar.len(), MAX_PER_DOMAIN);
        jar.store("http://h.example/", &format!("big={}", "x".repeat(MAX_BYTES)), NOW);
        assert_eq!(jar.len(), MAX_PER_DOMAIN);
    }

    #[test]
    fn junk_is_ignored() {
        let mut jar = Jar::default();
        for header in ["", "novalue", "=v", ";;;"] {
            jar.store("http://h.example/", header, NOW);
        }
        jar.store("ftp://h.example/", "a=1", NOW);
        assert!(jar.is_empty());
    }

    #[test]
    fn an_ip_host_keeps_its_cookies_to_itself() {
        let mut jar = Jar::default();
        jar.store("http://10.0.2.2/", "a=1; Domain=0.2.2", NOW);
        assert!(jar.is_empty());
        jar.store("http://10.0.2.2:8080/", "a=1", NOW);
        assert_eq!(sent(&jar, "http://10.0.2.2/"), "a=1");
    }
}

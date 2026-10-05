//! One `WEB:FETCH` serial line per fetch, with where its time went, so a slow
//! or failed page can be read off the log:
//!
//! `WEB:FETCH:<at>ms <status|FAIL> total=<ms> dns=<ms> tcp=<ms> tls=<ms>
//! wait=<ms> body=<ms> <bytes>B <url>`
//!
//! `at` is when the fetch started, in ms since the browser did ([`now_ms`]);
//! `wait` runs from the connection being ready to the response headers,
//! `body` from there to the end. A stage the fetch never reached is `-`.
//!
//! The marks are per thread: a fetch runs on one thread from the name lookup
//! ([`super::resolve`]) through the handshake ([`super::tls`]) to the last
//! body chunk ([`super::transfer`]).

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the first call (the browser calls it at start-up).
pub fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The stages a fetch passes, in order.
#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Resolved,
    Connected,
    Secured,
}

thread_local! {
    static MARKS: Cell<[Option<Instant>; 3]> = const { Cell::new([None; 3]) };
}

/// Notes that this thread's fetch just finished `stage`.
pub(crate) fn mark(stage: Stage) {
    MARKS.with(|marks| {
        let mut all = marks.get();
        all[stage as usize] = Some(Instant::now());
        marks.set(all);
    });
}

/// The timing of one fetch on this thread.
pub(crate) struct Timing {
    at: u64,
    start: Instant,
    headers: Option<Instant>,
    bytes: u64,
}

impl Timing {
    /// Starts timing a fetch on this thread.
    pub(crate) fn start() -> Timing {
        MARKS.with(|marks| marks.set([None; 3]));
        Timing {
            at: now_ms(),
            start: Instant::now(),
            headers: None,
            bytes: 0,
        }
    }

    pub(crate) fn headers(&mut self) {
        self.headers = Some(Instant::now());
    }

    pub(crate) fn add(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
    }

    /// Prints the fetch's line; `outcome` is its status or `FAIL`.
    pub(crate) fn report(&self, outcome: &str, url: &str) {
        println!("{}", self.line(outcome, url, Instant::now()));
    }

    fn line(&self, outcome: &str, url: &str, end: Instant) -> String {
        let [resolved, connected, secured] = MARKS.with(Cell::get);
        let ms = |from: Option<Instant>, to: Option<Instant>| match (from, to) {
            (Some(a), Some(b)) => (b.saturating_duration_since(a).as_millis()).to_string(),
            _ => "-".into(),
        };
        let start = Some(self.start);
        // The connection is ready after TLS for https:, after TCP for http:.
        let ready = secured.or(connected);
        let shown: String = crate::marker_text(&redacted(url))
            .chars()
            .take(160)
            .collect();
        format!(
            "WEB:FETCH:{}ms {outcome} total={} dns={} tcp={} tls={} wait={} body={} {}B {shown}",
            self.at,
            ms(start, Some(end)),
            ms(start, resolved),
            ms(resolved, connected),
            ms(connected, secured),
            ms(ready, self.headers),
            ms(self.headers, Some(end)),
            self.bytes,
        )
    }
}

/// `url` without what could be a secret: the user name and password, and
/// the query's values (`?token=…` becomes `?token=_`).
fn redacted(url: &str) -> String {
    let (url, query) = match url.split_once('?') {
        Some((url, query)) => (url, Some(query)),
        None => (url, None),
    };
    let mut out = match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(end);
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}{path}")
        }
        None => url.to_string(),
    };
    if let Some(query) = query {
        let query = query.split('#').next().unwrap_or("");
        let names: Vec<&str> = query
            .split('&')
            .map(|pair| pair.split('=').next().unwrap_or(""))
            .collect();
        out.push('?');
        out.push_str(&names.join("=_&"));
        if !query.is_empty() {
            out.push_str("=_");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_stages_are_dashes() {
        let timing = Timing::start();
        mark(Stage::Resolved);
        let line = timing.line("FAIL", "http://a.test/\nx", Instant::now());
        assert!(line.starts_with("WEB:FETCH:"), "{line}");
        assert!(line.contains(" FAIL total="), "{line}");
        assert!(
            line.contains(" tcp=- tls=- wait=- body=- 0B http://a.test/ x"),
            "{line}"
        );
    }

    #[test]
    fn stages_are_measured_between_marks() {
        let mut timing = Timing::start();
        mark(Stage::Resolved);
        mark(Stage::Connected);
        timing.headers();
        timing.add(10);
        let line = timing.line("200", "http://a.test/", Instant::now());
        assert!(line.contains(" 200 "), "{line}");
        let wait = line
            .split(" wait=")
            .nth(1)
            .and_then(|r| r.split(' ').next());
        assert!(line.contains(" tls=- wait="), "{line}");
        assert!(wait.is_some_and(|ms| ms.parse::<u64>().is_ok()), "{line}");
        assert!(line.contains(" 10B http://a.test/"), "{line}");
    }

    #[test]
    fn secrets_are_not_logged() {
        assert_eq!(
            redacted("https://me:pw@a.test/p?token=abc&x=1#f"),
            "https://a.test/p?token=_&x=_"
        );
        assert_eq!(redacted("http://a.test/@x?q"), "http://a.test/@x?q=_");
        assert_eq!(redacted("http://a.test/"), "http://a.test/");
    }
}

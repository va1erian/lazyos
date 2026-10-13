//! Keeping a list current: when it is due, the `curl` command that fetches it, what its answer
//! means, and the small record kept beside the list's file.
//!
//! The fetch runs as a child of the browser under a label of its own (`[processes.listupdate]`
//! in the manifest: outbound network, write access to the lists folder, nothing else), so it can
//! carry no cookies and no referrer, and a hostile list server reaches only that process.
//! Everything here is pure and host-tested; `worker.rs` runs it.

/// A list is refreshed after its own `! Expires:` line says so, within these bounds (seconds).
pub const DEFAULT_EXPIRES: u64 = 4 * 86_400;
pub const MIN_EXPIRES: u64 = 3_600;
pub const MAX_EXPIRES: u64 = 14 * 86_400;
/// After a failed fetch nothing is tried again for this long (seconds).
pub const RETRY_AFTER: u64 = 3_600;
/// A list that is not at least this long is not a list (an error page, a captive portal).
const MIN_LIST_BYTES: usize = 1_000;

/// What is kept about a list between runs (`<id>.meta` next to `<id>.txt`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The source's hash (`lists::source_hash`) the compiled parts below were made from.
    pub hash: String,
    /// The engine's ids of those parts.
    pub parts: Vec<String>,
    /// Rules and dropped filters of that conversion, for `about:blocker`.
    pub rules: usize,
    pub dropped: usize,
    /// When the source was last fetched, and when a fetch was last tried (seconds since the epoch).
    pub fetched: u64,
    pub attempted: u64,
    /// How long the source stays current, from its own `! Expires:` line.
    pub expires: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Meta {
    /// Reads the record; unknown or broken lines are ignored (the file is ours, but it sits in a
    /// folder a compromised network process can write).
    pub fn parse(text: &str) -> Meta {
        let mut meta = Meta::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let number = || value.parse::<u64>().ok();
            match key.trim() {
                "hash" if value.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    meta.hash = value.to_owned();
                }
                "parts" => {
                    meta.parts = value
                        .split(',')
                        .filter(|id| {
                            !id.is_empty()
                                && id.bytes().all(|b| {
                                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
                                })
                        })
                        .map(str::to_owned)
                        .collect();
                }
                "rules" => meta.rules = number().unwrap_or(0) as usize,
                "dropped" => meta.dropped = number().unwrap_or(0) as usize,
                "fetched" => meta.fetched = number().unwrap_or(0),
                "attempted" => meta.attempted = number().unwrap_or(0),
                "expires" => meta.expires = number().unwrap_or(0),
                "etag" if validator(value) => meta.etag = Some(value.to_owned()),
                "last-modified" if validator(value) => meta.last_modified = Some(value.to_owned()),
                _ => {}
            }
        }
        meta
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "hash={}\nparts={}\nrules={}\ndropped={}\nfetched={}\nattempted={}\nexpires={}\n",
            self.hash,
            self.parts.join(","),
            self.rules,
            self.dropped,
            self.fetched,
            self.attempted,
            self.expires
        );
        if let Some(etag) = &self.etag {
            out.push_str(&format!("etag={etag}\n"));
        }
        if let Some(modified) = &self.last_modified {
            out.push_str(&format!("last-modified={modified}\n"));
        }
        out
    }

    /// Whether a fetch is due at `now`: never fetched, or expired, and not tried too lately.
    pub fn due(&self, now: u64) -> bool {
        // A time in the future (the clock was set back, or a guest started at 1970 after a fetch
        // in 2026) cannot be trusted: it must not hold updates back until the clock catches up.
        let attempted = if self.attempted > now {
            0
        } else {
            self.attempted
        };
        if attempted > 0 && now < attempted.saturating_add(RETRY_AFTER) {
            return false;
        }
        let expires = self.expires.clamp(MIN_EXPIRES, MAX_EXPIRES);
        self.fetched == 0 || self.fetched > now || now >= self.fetched.saturating_add(expires)
    }
}

/// A header value that is safe to keep and to send back: printable ASCII, no quotes broken, short.
fn validator(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && value.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// The `! Expires: 4 days` (or `12 hours`) line of a list's header, in seconds; the default when
/// there is none or it makes no sense.
pub fn expires_of(list: &str) -> u64 {
    for line in list.lines().take(60) {
        let Some(rest) = line.trim().strip_prefix('!') else {
            continue;
        };
        let rest = rest.trim();
        let Some(value) = rest
            .get(..8)
            .filter(|h| h.eq_ignore_ascii_case("expires:"))
            .map(|_| rest[8..].trim())
        else {
            continue;
        };
        let mut words = value.split_whitespace();
        let Some(amount) = words.next().and_then(|n| n.parse::<u64>().ok()) else {
            return DEFAULT_EXPIRES;
        };
        let unit = words.next().unwrap_or("days").to_ascii_lowercase();
        let seconds = match unit.trim_end_matches('s').trim_end_matches(',') {
            "hour" | "hr" | "h" => 3_600,
            "day" | "d" => 86_400,
            _ => return DEFAULT_EXPIRES,
        };
        return amount
            .saturating_mul(seconds)
            .clamp(MIN_EXPIRES, MAX_EXPIRES);
    }
    DEFAULT_EXPIRES
}

/// What a fetch brought back: `curl -i` output (the headers of every response of a redirect
/// chain, then the final body).
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: Vec<u8>,
}

/// Parses `curl -i` output: `HTTP/...` header blocks, each ended by a blank line, the last one
/// the answer. `None` when it is not that.
pub fn parse_reply(raw: &[u8]) -> Option<Reply> {
    let mut rest = raw;
    let mut last: Option<Reply> = None;
    loop {
        if !rest.starts_with(b"HTTP/") {
            break;
        }
        let end = find(rest, b"\r\n\r\n")
            .map(|at| (at, 4))
            .or_else(|| find(rest, b"\n\n").map(|at| (at, 2)))?;
        let head = std::str::from_utf8(&rest[..end.0]).ok()?;
        let mut lines = head.lines();
        let status = lines
            .next()?
            .split_whitespace()
            .nth(1)?
            .parse::<u16>()
            .ok()?;
        let (mut etag, mut modified) = (None, None);
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let value = value.trim();
                match name.trim().to_ascii_lowercase().as_str() {
                    "etag" if validator(value) => etag = Some(value.to_owned()),
                    "last-modified" if validator(value) => modified = Some(value.to_owned()),
                    _ => {}
                }
            }
        }
        rest = &rest[end.0 + end.1..];
        last = Some(Reply {
            status,
            etag,
            last_modified: modified,
            body: Vec::new(),
        });
        // A redirect's body is empty; an intermediate header block is followed by the next one.
        if !(300..400).contains(&status) && status != 100 {
            break;
        }
    }
    let mut reply = last?;
    reply.body = rest.to_vec();
    Some(reply)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Whether `text` is a filter list: its header, and a body big enough to be one.
pub fn looks_like_list(text: &[u8]) -> bool {
    text.len() >= MIN_LIST_BYTES
        && text.get(..64).or(Some(text)).is_some_and(|head| {
            String::from_utf8_lossy(head)
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("[adblock")
        })
}

/// The `curl` arguments that fetch `url` into `out`: quiet, follow a few redirects, give up on an
/// HTTP error, and ask only for what changed. No cookie is sent (curl keeps none) and no referrer.
pub fn curl_args(url: &str, out: &str, meta: &Meta) -> Vec<String> {
    let mut args: Vec<String> = [
        "-sS",
        "-L",
        "-f",
        "-i",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "30",
        "-m",
        "600",
        "-A",
        "LazyWeb+ filter list update",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if let Some(etag) = &meta.etag {
        args.push("-H".into());
        args.push(format!("If-None-Match: {etag}"));
    }
    if let Some(modified) = &meta.last_modified {
        args.push("-H".into());
        args.push(format!("If-Modified-Since: {modified}"));
    }
    args.push("-o".into());
    args.push(out.to_owned());
    args.push(url.to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_and_ignores_what_it_does_not_know() {
        let meta = Meta {
            hash: "00ff".into(),
            parts: vec!["easylist-00ff-0".into(), "easylist-00ff-1".into()],
            rules: 56_926,
            dropped: 24_851,
            fetched: 1_791_000_000,
            attempted: 1_791_000_100,
            expires: 345_600,
            etag: Some("\"abc\"".into()),
            last_modified: Some("Tue, 13 Oct 2026 12:47:00 GMT".into()),
        };
        assert_eq!(Meta::parse(&meta.render()), meta);
        let junk =
            Meta::parse("hash=zz\nparts=../x,ok-1\nrules=-4\nunknown=1\nno equals\netag=\u{1}\n");
        assert_eq!(junk.hash, "");
        assert_eq!(junk.parts, ["ok-1"]);
        assert_eq!(junk.rules, 0);
        assert_eq!(junk.etag, None);
    }

    #[test]
    fn a_fetch_is_due_when_the_list_expired_and_not_tried_lately() {
        let mut meta = Meta::default();
        assert!(meta.due(100), "never fetched");
        meta.fetched = 1_000_000;
        meta.expires = DEFAULT_EXPIRES;
        assert!(!meta.due(1_000_000 + DEFAULT_EXPIRES - 1));
        assert!(meta.due(1_000_000 + DEFAULT_EXPIRES));
        meta.attempted = 1_000_000 + DEFAULT_EXPIRES;
        assert!(
            !meta.due(1_000_000 + DEFAULT_EXPIRES + 10),
            "a failed try waits"
        );
        assert!(meta.due(1_000_000 + DEFAULT_EXPIRES + RETRY_AFTER));
        // A list that asks for a refresh every second is refreshed hourly at most.
        meta.attempted = 0;
        meta.expires = 1;
        assert!(!meta.due(1_000_000 + MIN_EXPIRES - 1));
        assert!(meta.due(1_000_000 + MIN_EXPIRES));
    }

    #[test]
    fn the_expires_header_is_read_in_days_or_hours() {
        assert_eq!(
            expires_of("[Adblock Plus 2.0]\n! Expires: 4 days (update frequency)\n"),
            4 * 86_400
        );
        assert_eq!(expires_of("! Expires: 12 hours\n"), 12 * 3_600);
        assert_eq!(expires_of("! expires: 1 day\n"), 86_400);
        assert_eq!(expires_of("! Expires: 900 days\n"), MAX_EXPIRES);
        assert_eq!(expires_of("! Expires: soon\n"), DEFAULT_EXPIRES);
        assert_eq!(expires_of("! Title: x\n||a^\n"), DEFAULT_EXPIRES);
        assert_eq!(expires_of("! Expires: 4 weeks\n"), DEFAULT_EXPIRES);
    }

    #[test]
    fn a_reply_is_the_last_header_block_and_the_body_after_it() {
        let raw = b"HTTP/1.1 301 Moved\r\nLocation: /x\r\n\r\nHTTP/2 200\r\nETag: \"v2\"\r\nLast-Modified: Tue, 13 Oct 2026 12:47:00 GMT\r\ncontent-length: 4\r\n\r\nbody";
        let reply = parse_reply(raw).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.etag.as_deref(), Some("\"v2\""));
        assert_eq!(
            reply.last_modified.as_deref(),
            Some("Tue, 13 Oct 2026 12:47:00 GMT")
        );
        assert_eq!(reply.body, b"body");
        let not_modified =
            parse_reply(b"HTTP/1.1 304 Not Modified\r\nETag: \"v2\"\r\n\r\n").unwrap();
        assert_eq!((not_modified.status, not_modified.body.len()), (304, 0));
        assert_eq!(parse_reply(b"not http"), None);
        assert_eq!(parse_reply(b"HTTP/1.1 200 OK\r\nno end"), None);
    }

    #[test]
    fn a_list_has_its_header_and_some_size() {
        let mut list = b"[Adblock Plus 2.0]\n".to_vec();
        list.extend(std::iter::repeat_n(b'x', 2_000));
        assert!(looks_like_list(&list));
        assert!(!looks_like_list(b"[Adblock Plus 2.0]\n||a^\n"), "too short");
        let mut page = b"<!DOCTYPE html><html>".to_vec();
        page.extend(std::iter::repeat_n(b' ', 2_000));
        assert!(!looks_like_list(&page));
        assert!(!looks_like_list(b""));
    }

    #[test]
    fn the_command_asks_only_for_what_changed_and_sends_no_credentials() {
        let mut meta = Meta::default();
        let args = curl_args(
            "https://easylist.to/easylist/easylist.txt",
            "/p/easylist.dl",
            &meta,
        );
        assert_eq!(
            args.last().unwrap(),
            "https://easylist.to/easylist/easylist.txt"
        );
        assert!(args.windows(2).any(|w| w == ["-o", "/p/easylist.dl"]));
        assert!(!args.iter().any(|a| a.contains("If-None-Match")));
        assert!(!args
            .iter()
            .any(|a| a == "-b" || a == "--cookie" || a == "-e" || a == "-k"));
        meta.etag = Some("\"abc\"".into());
        meta.last_modified = Some("Tue, 13 Oct 2026 12:47:00 GMT".into());
        let args = curl_args("https://x/", "/o", &meta);
        assert!(args.contains(&"If-None-Match: \"abc\"".to_owned()));
        assert!(args.contains(&"If-Modified-Since: Tue, 13 Oct 2026 12:47:00 GMT".to_owned()));
    }
}

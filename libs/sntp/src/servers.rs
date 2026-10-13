//! The configured server list (`sys/time/ntp/servers`): space or comma
//! separated `host` or `host:port` entries. The text comes from `confd`
//! (an administrator's), but it is parsed as untrusted: a bad entry is
//! skipped, never an error for the rest.

use crate::PORT;

/// Servers asked per round; more entries are ignored.
pub const MAX_SERVERS: usize = 4;

/// The longest host name (DNS).
const MAX_HOST: usize = 253;

/// One entry: a name or dotted quad, and the UDP port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Server<'a> {
    pub host: &'a str,
    pub port: u16,
}

fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST
        && !host.starts_with(['.', '-'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn entry(text: &str) -> Option<Server<'_>> {
    let (host, port) = match text.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok().filter(|&p| p != 0)?),
        None => (text, PORT),
    };
    host_ok(host).then_some(Server { host, port })
}

/// The usable entries of `text`, in order, at most [`MAX_SERVERS`]. IPv6
/// literals (more than one colon) are not supported and skipped.
pub fn parse(text: &str) -> impl Iterator<Item = Server<'_>> {
    text.split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(entry)
        .take(MAX_SERVERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<(String, u16)> {
        parse(text).map(|s| (s.host.to_string(), s.port)).collect()
    }

    #[test]
    fn names_ports_and_separators() {
        assert_eq!(
            all("pool.ntp.org time.example.net:1123,10.0.2.2"),
            [
                ("pool.ntp.org".to_string(), 123),
                ("time.example.net".to_string(), 1123),
                ("10.0.2.2".to_string(), 123),
            ]
        );
        assert!(all("").is_empty());
        assert!(all(" , ,\t").is_empty());
    }

    #[test]
    fn bad_entries_are_skipped_not_fatal() {
        assert_eq!(
            all("bad host:0 ok.example :123 a:b:c -x .y z:99999 fine"),
            [
                ("bad".to_string(), 123),
                ("ok.example".to_string(), 123),
                ("fine".to_string(), 123),
            ]
        );
        assert!(all("[::1]:123 fe80::1").is_empty());
        assert!(all("evil/path user@host host?x").is_empty());
    }

    #[test]
    fn at_most_four() {
        assert_eq!(all("a b c d e f").len(), MAX_SERVERS);
    }

    #[test]
    fn overlong_names_are_refused() {
        let long = "a".repeat(MAX_HOST + 1);
        assert!(all(&long).is_empty());
        assert_eq!(all(&"a".repeat(MAX_HOST)).len(), 1);
    }
}

//! Name lookup on the calling (fetch) thread.
//!
//! ureq's default resolver moves a lookup to a helper thread whenever a
//! timeout applies, so that it can stop waiting. On LazyOS a thread has its
//! own descriptor table, so musl's resolver socket would live in a thread
//! nobody else can see and an abandoned lookup would leave that thread
//! behind. This one asks `getaddrinfo` (through std) directly; musl bounds
//! it with its resolv.conf timeouts.
//!
//! Answers are kept for [`CACHE_FOR`]: a page asks for the same few hosts
//! dozens of times (every style sheet and image is a fetch of its own), and
//! each lookup is a round trip through `netd`.

use std::collections::HashMap;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;
use ureq::Error;

use super::trace::{self, Stage};

/// ureq holds at most this many addresses per lookup.
const MAX_ADDRS: usize = 16;

/// How long a lookup's answer is reused. Short, since the resolver does not
/// tell us the record's TTL, but long enough to cover a page's load.
const CACHE_FOR: Duration = Duration::from_secs(60);

/// At most this many names are kept; a full cache starts over.
const CACHE_NAMES: usize = 64;

/// Recent answers by `host:port`, with when each was looked up. Only
/// successful lookups are kept.
type Answers = HashMap<String, (Instant, Vec<SocketAddr>)>;

static CACHE: Mutex<Option<Answers>> = Mutex::new(None);

fn cached(target: &str, now: Instant) -> Option<Vec<SocketAddr>> {
    let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let (at, addrs) = cache.as_ref()?.get(target)?;
    (now.duration_since(*at) < CACHE_FOR).then(|| addrs.clone())
}

fn remember(target: &str, addrs: &[SocketAddr], now: Instant) {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let map = cache.get_or_insert_with(HashMap::new);
    if map.len() >= CACHE_NAMES && !map.contains_key(target) {
        map.clear();
    }
    map.insert(target.to_string(), (now, addrs.to_vec()));
}

/// The addresses of `target` (`host:port`), IPv4 first (LazyOS has no IPv6
/// yet; std connects in this order).
fn lookup(target: &str) -> Option<Vec<SocketAddr>> {
    let now = Instant::now();
    if let Some(addrs) = cached(target, now) {
        return Some(addrs);
    }
    let mut addrs: Vec<SocketAddr> = target.to_socket_addrs().ok()?.collect();
    addrs.sort_by_key(SocketAddr::is_ipv6);
    addrs.truncate(MAX_ADDRS);
    if addrs.is_empty() {
        return None;
    }
    remember(target, &addrs, now);
    Some(addrs)
}

pub(crate) struct InlineResolver;

impl fmt::Debug for InlineResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InlineResolver")
    }
}

impl Resolver for InlineResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let bad = || Error::BadUri(uri.to_string());
        let (scheme, authority) = uri.scheme().zip(uri.authority()).ok_or_else(bad)?;
        let target = DefaultResolver::host_and_port(scheme, authority).ok_or_else(bad)?;
        let addrs = lookup(&target).ok_or(Error::HostNotFound)?;
        let mut out = self.empty();
        for addr in addrs {
            out.push(addr);
        }
        trace::mark(Stage::Resolved);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_reused_until_they_expire() {
        let addr: SocketAddr = "192.0.2.7:443".parse().unwrap();
        let then = Instant::now();
        remember("cache-test.invalid:443", &[addr], then);
        assert_eq!(cached("cache-test.invalid:443", then), Some(vec![addr]));
        assert_eq!(cached("cache-test.invalid:443", then + CACHE_FOR), None);
        assert_eq!(cached("other.invalid:443", then), None);
    }

    #[test]
    fn literal_addresses_resolve() {
        let addrs = lookup("127.0.0.1:8080").unwrap();
        assert_eq!(addrs, ["127.0.0.1:8080".parse::<SocketAddr>().unwrap()]);
    }
}

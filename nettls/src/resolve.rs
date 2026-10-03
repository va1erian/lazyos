//! Name resolution on the calling thread.
//!
//! ureq 3.4's `DefaultResolver` calls `to_socket_addrs` on a freshly spawned
//! thread whenever any timeout applies to the resolve step (`resolve_async`
//! in `unversioned/resolver.rs`), so that it can stop waiting. On LazyOS a
//! thread gets its own descriptor table (docs/tls-plan.md §4.2): musl's
//! resolver would open its UDP socket in a thread that the rest of the
//! program cannot see into, and an abandoned lookup would leave that thread
//! running. This resolver never spawns: it asks musl's `getaddrinfo` (via
//! `std`) directly, which is bounded by musl's own resolv.conf timeouts
//! (5 s x 2 attempts by default). The `--connect-timeout` and `--max-time`
//! limits still apply to connecting and to the transfer.

use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;
use ureq::Error;

/// ureq keeps at most this many addresses per lookup (`MAX_ADDRS`).
const MAX_ADDRS: usize = 16;

/// Resolves synchronously, IPv4 first (LazyOS has no IPv6 stack yet, and
/// `std` tries the addresses in order).
#[derive(Default)]
pub struct SyncResolver;

impl fmt::Debug for SyncResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SyncResolver")
    }
}

impl Resolver for SyncResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
            return Err(Error::BadUri(uri.to_string()));
        };
        let target = DefaultResolver::host_and_port(scheme, authority)
            .ok_or_else(|| Error::BadUri(uri.to_string()))?;
        let found = target.to_socket_addrs().map_err(|_| Error::HostNotFound)?;
        let mut result = self.empty();
        for addr in ipv4_first(found).into_iter().take(MAX_ADDRS) {
            result.push(addr);
        }
        if result.is_empty() {
            return Err(Error::HostNotFound);
        }
        Ok(result)
    }
}

/// `addrs` with every IPv4 address before every IPv6 one, order otherwise
/// kept.
pub fn ipv4_first(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let mut all: Vec<SocketAddr> = addrs.into_iter().collect();
    all.sort_by_key(|a| a.is_ipv6());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_sorts_first_stably() {
        let list: Vec<SocketAddr> = ["[::1]:1", "10.0.0.1:1", "[::2]:1", "10.0.0.2:1"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let sorted = ipv4_first(list);
        let text: Vec<String> = sorted.iter().map(|a| a.to_string()).collect();
        assert_eq!(text, ["10.0.0.1:1", "10.0.0.2:1", "[::1]:1", "[::2]:1"]);
    }

    #[test]
    fn resolves_localhost_without_threads() {
        let uri: ureq::http::Uri = "http://127.0.0.1:8080/".parse().unwrap();
        let config = Config::default();
        let addrs = SyncResolver
            .resolve(
                &uri,
                &config,
                NextTimeout {
                    after: ureq::unversioned::transport::time::Duration::from_secs(1),
                    reason: ureq::Timeout::Resolve,
                },
            )
            .unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "127.0.0.1:8080");
    }
}

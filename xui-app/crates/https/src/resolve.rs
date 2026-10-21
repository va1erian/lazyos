//! Name lookup on the calling thread.
//!
//! ureq's default resolver moves a lookup to a helper thread whenever a
//! timeout applies. On LazyOS a thread has its own descriptor table, so the
//! resolver's socket would live in a thread nobody else can see. This one
//! asks `getaddrinfo` (through std) directly. Adapted from the fetcher of
//! LazyWeb (the Blitz browser, removed), without its answer cache.

use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;
use ureq::Error;

/// ureq holds at most this many addresses per lookup.
const MAX_ADDRS: usize = 16;

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
        // IPv4 first: LazyOS has no IPv6 yet and std connects in this order.
        let mut addrs: Vec<SocketAddr> = target
            .to_socket_addrs()
            .map_err(|_| Error::HostNotFound)?
            .collect();
        addrs.sort_by_key(SocketAddr::is_ipv6);
        addrs.truncate(MAX_ADDRS);
        if addrs.is_empty() {
            return Err(Error::HostNotFound);
        }
        let mut out = self.empty();
        for addr in addrs {
            out.push(addr);
        }
        Ok(out)
    }
}

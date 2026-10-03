//! Name lookup on the calling (fetch) thread.
//!
//! ureq's default resolver moves a lookup to a helper thread whenever a
//! timeout applies, so that it can stop waiting. On LazyOS a thread has its
//! own descriptor table, so musl's resolver socket would live in a thread
//! nobody else can see and an abandoned lookup would leave that thread
//! behind. This one asks `getaddrinfo` (through std) directly; musl bounds
//! it with its resolv.conf timeouts.

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
        let mut addrs: Vec<SocketAddr> = target
            .to_socket_addrs()
            .map_err(|_| Error::HostNotFound)?
            .collect();
        // IPv4 first (LazyOS has no IPv6 yet); std connects in this order.
        addrs.sort_by_key(SocketAddr::is_ipv6);
        let mut out = self.empty();
        for addr in addrs.into_iter().take(MAX_ADDRS) {
            out.push(addr);
        }
        if out.is_empty() {
            return Err(Error::HostNotFound);
        }
        Ok(out)
    }
}

//! What one interface can tell the layer above it about where it can send:
//! the questions `Net` asks every interface before it picks one.

use super::sockets::Kind;
use super::Stack;
use crate::config::same_subnet;

/// How an interface would reach a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    /// The destination is on the interface's own subnet.
    OnLink,
    /// Only through the interface's default gateway.
    Gateway,
}

impl Stack {
    /// How this interface would send to `dst`, if it can at all: it needs an
    /// address, and a destination off its subnet needs a gateway.
    pub fn route_class(&self, dst: [u8; 4]) -> Option<RouteClass> {
        let addr = self.state.addr?;
        if same_subnet(dst, addr, self.state.prefix_len) {
            Some(RouteClass::OnLink)
        } else {
            self.state.gateway.map(|_| RouteClass::Gateway)
        }
    }

    /// Whether a socket of this interface holds `port` for `kind` (a stream
    /// still closing counts: the wire still knows the port).
    pub fn socket_port_in_use(&self, kind: Kind, port: u16) -> bool {
        self.socks.port_in_use(&self.sockets, kind, port)
    }
}

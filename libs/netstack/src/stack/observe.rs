//! Per-poll bookkeeping for stream sockets: what the wire did to each one
//! (connected, refused, reset, peer closed), so the state and the counters are
//! right before a client asks.

use smoltcp::socket::tcp;

use super::sockets::SocketCounters;
use super::Stack;

impl Stack {
    /// Open sockets, all owners.
    pub fn socket_open_count(&self) -> usize {
        self.socks.open_count()
    }

    /// Streams closed by their owner and still finishing on the wire.
    pub fn socket_closing(&self) -> usize {
        self.socks.closing_count()
    }

    /// Counters over the life of the socket table.
    pub fn socket_counters(&self) -> SocketCounters {
        *self.socks.counters()
    }

    /// Who owns at least one socket, each once.
    pub fn socket_owners(&self) -> alloc::vec::Vec<u64> {
        self.socks.owners()
    }

    /// Once per poll: notice what the wire did to each stream, so the
    /// per-socket state and the counters are right before clients ask.
    pub(super) fn observe_streams(&mut self, now_ms: i64) {
        let (mut refused, mut connected, mut resets) = (0u64, 0u64, 0u64);
        for (state, handle) in self.socks.streams_mut() {
            let socket = self.sockets.get::<tcp::Socket>(handle);
            let tcp_state = socket.state();
            if state.connecting {
                match tcp_state {
                    tcp::State::Closed => {
                        state.connecting = false;
                        state.refused = true;
                        refused += 1;
                    }
                    tcp::State::SynSent => {}
                    _ => {
                        state.connecting = false;
                        state.established = true;
                        connected += 1;
                    }
                }
            }
            if state.established {
                if matches!(
                    tcp_state,
                    tcp::State::CloseWait
                        | tcp::State::Closing
                        | tcp::State::LastAck
                        | tcp::State::TimeWait
                ) || (!socket.may_recv() && tcp_state != tcp::State::Closed)
                {
                    state.fin_seen = true;
                }
                if tcp_state == tcp::State::Closed && !state.fin_seen && !state.reset {
                    state.reset = true;
                    resets += 1;
                }
            }
        }
        self.socks.counters.refused += refused;
        self.socks.counters.connected += connected;
        self.socks.counters.resets += resets;
        self.socks.reap_closing(&mut self.sockets, now_ms);
    }
}

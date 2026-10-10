//! Counters over all interfaces: the live ones plus what removed ones had.

use crate::device::DeviceStats;
use crate::stack::{Counters, SocketCounters};

use super::Net;

pub(super) fn add_counters(a: &Counters, b: &Counters) -> Counters {
    Counters {
        leases: a.leases + b.leases,
        lease_losses: a.lease_losses + b.lease_losses,
        pings_sent: a.pings_sent + b.pings_sent,
        pings_answered: a.pings_answered + b.pings_answered,
        pings_timed_out: a.pings_timed_out + b.pings_timed_out,
        lookups_sent: a.lookups_sent + b.lookups_sent,
        lookups_answered: a.lookups_answered + b.lookups_answered,
        lookups_failed: a.lookups_failed + b.lookups_failed,
    }
}

pub(super) fn add_sockets(a: &SocketCounters, b: &SocketCounters) -> SocketCounters {
    SocketCounters {
        opened: a.opened + b.opened,
        closed: a.closed + b.closed,
        reclaimed: a.reclaimed + b.reclaimed,
        connected: a.connected + b.connected,
        accepted: a.accepted + b.accepted,
        refused: a.refused + b.refused,
        resets: a.resets + b.resets,
        tx_bytes: a.tx_bytes + b.tx_bytes,
        rx_bytes: a.rx_bytes + b.rx_bytes,
        tx_datagrams: a.tx_datagrams + b.tx_datagrams,
        rx_datagrams: a.rx_datagrams + b.rx_datagrams,
    }
}

impl Net {
    /// Stack counters, all interfaces together.
    pub fn counters(&self) -> Counters {
        self.units()
            .fold(self.retired.stack, |sum, (_, unit)| {
                add_counters(&sum, unit.stack.counters())
            })
    }

    /// Ring counters, all interfaces together.
    pub fn device_stats(&self) -> DeviceStats {
        self.units().fold(DeviceStats::default(), |sum, (_, unit)| {
            let d = unit.stack.device_stats();
            DeviceStats {
                rx_frames: sum.rx_frames + d.rx_frames,
                tx_frames: sum.tx_frames + d.tx_frames,
                rx_bytes: sum.rx_bytes + d.rx_bytes,
                tx_bytes: sum.tx_bytes + d.tx_bytes,
                tx_dropped: sum.tx_dropped + d.tx_dropped,
                rx_bad_length: sum.rx_bad_length + d.rx_bad_length,
            }
        })
    }

    /// Socket counters. What a socket *is* (opened, closed, reclaimed,
    /// accepted) is counted once here, not once per replica; what the wire did
    /// to it comes from the interfaces.
    pub fn socket_counters(&self) -> SocketCounters {
        let wire = self.units().fold(self.retired.sockets, |sum, (_, unit)| {
            add_sockets(&sum, &unit.stack.socket_counters())
        });
        SocketCounters {
            opened: self.sockets.opened,
            closed: self.sockets.closed,
            reclaimed: self.sockets.reclaimed,
            accepted: self.sockets.accepted,
            ..wire
        }
    }

    /// Streams closed by their owners and still finishing on the wire.
    pub fn socket_closing(&self) -> usize {
        self.units().map(|(_, unit)| unit.stack.socket_closing()).sum()
    }
}

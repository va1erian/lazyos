//! `Net`: several interfaces under one socket table (docs/wifi-prerequisites-plan.md
//! section 3.1).
//!
//! Each interface is a [`Stack`]: a smoltcp `Interface` over its NIC's rings
//! with its own socket set, DHCP client, echo socket and resolver socket.
//! smoltcp cannot share a socket set between interfaces (`tests/shared_socket_set.rs`:
//! the first interface to poll takes any ready socket's packet), so the layer
//! above does the choosing, and this is it:
//!
//! * **Interface choice.** A stream socket takes an interface when it
//!   connects, a datagram socket for every datagram it sends, a ping or a
//!   lookup when it starts: the usable interface whose subnet holds the
//!   destination (lowest metric first), else the one with the lowest-metric
//!   default route (wired 100, wireless 600, so a cable wins).
//! * **Replicas.** A listener, or a datagram socket bound to any address,
//!   must serve every interface, so it owns one smoltcp socket per
//!   interface (a replica), added for an interface that appears later. A
//!   socket bound to one interface's address, or connected, has one.
//! * **Ids.** Callers see `Net` socket ids only; the ids of the per-interface
//!   tables stay inside. Quotas, owners and counters are `Net`'s, as before.
//! * **Interfaces come and go.** [`Net::add_interface`] and
//!   [`Net::remove_interface`] at any time; a connection on a removed
//!   interface is reset, a wildcard socket just loses its replica there.
//!
//! The service glue (`netd`) owns everything that touches the machine; nothing
//! here takes a syscall.

use alloc::string::String;
use alloc::vec::Vec;

use crate::stack::{Counters, SocketCounters, Stack};

mod ops;
mod ops_dgram;
mod ops_stream;
mod probe;
mod route;
mod stats;
mod table;
#[cfg(test)]
mod tests;

pub use probe::{NetLookupResult, NetPingResult};
pub use route::Route;

use probe::Probes;
use table::Table;

/// Interfaces at once.
pub const MAX_INTERFACES: usize = 8;

/// What the interface is; it sets the default-route metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfKind {
    Wired,
    Wireless,
}

impl IfKind {
    /// The default-route metric: lower wins (the NetworkManager convention).
    pub const fn metric(self) -> u32 {
        match self {
            IfKind::Wired => 100,
            IfKind::Wireless => 600,
        }
    }
}

/// Why an interface could not be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// [`MAX_INTERFACES`] are present.
    Full,
    /// An interface of this name exists, or the name is empty or too long.
    BadName,
}

/// One interface.
pub struct Unit {
    pub name: String,
    pub kind: IfKind,
    /// The default-route metric ([`IfKind::metric`] unless changed).
    pub metric: u32,
    pub stack: Stack,
    /// The driver's rings are attached.
    attached: bool,
    /// The driver reports the link up.
    link: bool,
}

impl Unit {
    /// Whether the interface can carry traffic: rings attached and link up.
    pub fn up(&self) -> bool {
        self.attached && self.link
    }

    pub fn link(&self) -> bool {
        self.link
    }

    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Up and holding an address: a candidate for new traffic.
    pub(crate) fn usable(&self) -> bool {
        self.up() && self.stack.state().addr.is_some()
    }
}

pub struct Net {
    units: Vec<Option<Unit>>,
    table: Table,
    probes: Probes,
    /// The table-level counters (`opened`, `closed`, `reclaimed`, `accepted`
    /// are counted here, once per caller-visible socket).
    sockets: SocketCounters,
    /// What interfaces that were removed had counted.
    retired: Retired,
    /// Bumped whenever the set of interfaces, a link or the primary changes.
    generation: u64,
    /// Rotates which replica is tried first, so none starves.
    turn: usize,
}

#[derive(Default)]
struct Retired {
    sockets: SocketCounters,
    stack: Counters,
}

impl Net {
    /// An empty `Net`. `seed` seeds the ephemeral port offset (the caller
    /// draws it from the kernel CSPRNG).
    pub fn new(seed: u64) -> Net {
        Net {
            units: Vec::new(),
            table: Table::new(seed),
            probes: Probes::new(),
            sockets: SocketCounters::default(),
            retired: Retired::default(),
            generation: 0,
            turn: 0,
        }
    }

    /// Add an interface and give it a slot. Wildcard sockets get a replica on
    /// it. `link` is the link state the driver reported; the stack's device
    /// says whether the rings are attached yet.
    pub fn add_interface(
        &mut self,
        name: &str,
        kind: IfKind,
        stack: Stack,
        link: bool,
    ) -> Result<usize, AddError> {
        if name.is_empty() || name.len() > 15 || self.slot_of(name).is_some() {
            return Err(AddError::BadName);
        }
        let unit = Unit {
            name: String::from(name),
            kind,
            metric: kind.metric(),
            attached: stack.device().is_attached(),
            stack,
            link,
        };
        let slot = match self.units.iter().position(Option::is_none) {
            Some(slot) => slot,
            None if self.units.len() < MAX_INTERFACES => {
                self.units.push(None);
                self.units.len() - 1
            }
            None => return Err(AddError::Full),
        };
        self.units[slot] = Some(unit);
        self.generation += 1;
        self.spread_to(slot);
        Ok(slot)
    }

    /// Remove an interface. Its connections are reset, its wildcard replicas
    /// vanish, its pings and lookups end as timed out. Returns its name.
    pub fn remove_interface(&mut self, slot: usize) -> Option<String> {
        let unit = self.units.get_mut(slot)?.take()?;
        self.generation += 1;
        self.retired.sockets = stats::add_sockets(&self.retired.sockets, &unit.stack.socket_counters());
        self.retired.stack = stats::add_counters(&self.retired.stack, unit.stack.counters());
        self.forget_unit(slot);
        Some(unit.name)
    }

    pub fn slot_of(&self, name: &str) -> Option<usize> {
        self.units().find(|(_, unit)| unit.name == name).map(|(slot, _)| slot)
    }

    pub fn unit(&self, slot: usize) -> Option<&Unit> {
        self.units.get(slot)?.as_ref()
    }

    pub fn unit_mut(&mut self, slot: usize) -> Option<&mut Unit> {
        self.units.get_mut(slot)?.as_mut()
    }

    /// Every interface, by slot.
    pub fn units(&self) -> impl Iterator<Item = (usize, &Unit)> {
        self.units
            .iter()
            .enumerate()
            .filter_map(|(slot, unit)| Some((slot, unit.as_ref()?)))
    }

    /// Every interface, mutably, by slot.
    pub fn units_mut(&mut self) -> impl Iterator<Item = (usize, &mut Unit)> {
        self.units
            .iter_mut()
            .enumerate()
            .filter_map(|(slot, unit)| Some((slot, unit.as_mut()?)))
    }

    /// The rings of `slot`'s driver were attached or dropped. Traffic stops
    /// while they are not, but the lease is kept: a driver that restarts
    /// quickly costs no new DHCP exchange.
    pub fn set_attached(&mut self, slot: usize, attached: bool) {
        if let Some(unit) = self.unit_mut(slot) {
            if unit.attached != attached {
                unit.attached = attached;
                self.generation += 1;
            }
        }
    }

    /// The driver reported a link change. Down keeps the lease; up starts
    /// DHCP over (smoltcp has no INIT-REBOOT), because the link may have come
    /// back on another network. A static interface keeps its address.
    pub fn set_link(&mut self, slot: usize, link: bool) {
        let Some(unit) = self.unit_mut(slot) else {
            return;
        };
        if unit.link == link {
            return;
        }
        unit.link = link;
        if link {
            unit.stack.renew();
        }
        self.generation += 1;
    }

    /// Change `slot`'s default-route metric.
    pub fn set_metric(&mut self, slot: usize, metric: u32) {
        if let Some(unit) = self.unit_mut(slot) {
            if unit.metric != metric {
                unit.metric = metric;
                self.generation += 1;
            }
        }
    }

    /// Changes whenever the interface set, a link or an attachment does. Each
    /// interface's own address changes are in `Stack::epoch`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Ask every DHCP interface for a fresh lease.
    pub fn renew(&mut self) {
        for (_, unit) in self.units_mut() {
            unit.stack.renew();
        }
    }

    /// One round of work for every interface at stack time `now_ms`.
    pub fn poll(&mut self, now_ms: i64) {
        for (_, unit) in self.units_mut() {
            unit.stack.poll(now_ms);
        }
    }

    /// Milliseconds until some interface needs a `poll` even if nothing
    /// arrives.
    pub fn poll_delay_ms(&mut self, now_ms: i64) -> Option<u64> {
        self.units_mut()
            .filter_map(|(_, unit)| unit.stack.poll_delay_ms(now_ms))
            .min()
    }
}

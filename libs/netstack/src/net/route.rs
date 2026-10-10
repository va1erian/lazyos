//! Which interface carries what: the route lookup, the primary interface and
//! the resolvers it supplies.

use alloc::vec::Vec;

use crate::stack::RouteClass;

use super::Net;

/// One route, as the `Routes` call lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// The interface's slot.
    pub slot: usize,
    /// All zero with `prefix_len` 0 is the default route.
    pub dest: [u8; 4],
    pub prefix_len: u8,
    /// All zero for an on-link route.
    pub gateway: [u8; 4],
    pub metric: u32,
}

impl Net {
    /// The usable interface for a packet to `dst`: the lowest-metric one whose
    /// subnet holds it, else the lowest-metric one with a default route.
    /// Ties go to the lower slot, so the choice never flaps.
    pub fn route_for(&self, dst: [u8; 4]) -> Option<usize> {
        self.route_among(dst, |_| true)
    }

    /// [`Net::route_for`] limited to the slots `allow` accepts.
    pub(super) fn route_among(&self, dst: [u8; 4], allow: impl Fn(usize) -> bool) -> Option<usize> {
        self.units()
            .filter(|(slot, unit)| unit.usable() && allow(*slot))
            .filter_map(|(slot, unit)| {
                let rank = match unit.stack.route_class(dst)? {
                    RouteClass::OnLink => 0u8,
                    RouteClass::Gateway => 1,
                };
                Some((rank, unit.metric, slot))
            })
            .min()
            .map(|(_, _, slot)| slot)
    }

    /// The interface holding the best default route right now: new
    /// connections and the resolvers come from it.
    pub fn primary(&self) -> Option<usize> {
        self.units()
            .filter(|(_, unit)| unit.usable() && unit.stack.state().gateway.is_some())
            .map(|(slot, unit)| (unit.metric, slot))
            .min()
            .map(|(_, slot)| slot)
    }

    /// Where lookups go: the primary interface, or failing a default route
    /// anywhere the usable interface of lowest metric that has a resolver.
    pub(super) fn dns_slot(&self) -> Option<usize> {
        self.primary()
            .filter(|slot| self.has_resolver(*slot))
            .or_else(|| {
                self.units()
                    .filter(|(slot, unit)| unit.usable() && self.has_resolver(*slot))
                    .map(|(slot, unit)| (unit.metric, slot))
                    .min()
                    .map(|(_, slot)| slot)
            })
    }

    fn has_resolver(&self, slot: usize) -> bool {
        self.unit(slot)
            .is_some_and(|unit| !unit.stack.state().dns.is_empty())
    }

    /// The resolvers to write to `resolv.conf`: those of [`Net::dns_slot`].
    pub fn dns_servers(&self) -> Vec<[u8; 4]> {
        self.dns_slot()
            .and_then(|slot| self.unit(slot))
            .map(|unit| unit.stack.state().dns.clone())
            .unwrap_or_default()
    }

    /// Every on-link route and default route of the interfaces that hold an
    /// address, the default routes after the on-link ones, best metric first.
    pub fn routes(&self) -> Vec<Route> {
        let mut routes = Vec::new();
        for (slot, unit) in self.units() {
            let state = unit.stack.state();
            let Some(addr) = state.addr else { continue };
            routes.push(Route {
                slot,
                dest: network_of(addr, state.prefix_len),
                prefix_len: state.prefix_len,
                gateway: [0; 4],
                metric: 0,
            });
        }
        let mut defaults: Vec<Route> = self
            .units()
            .filter_map(|(slot, unit)| {
                Some(Route {
                    slot,
                    dest: [0; 4],
                    prefix_len: 0,
                    gateway: unit.stack.state().gateway?,
                    metric: unit.metric,
                })
            })
            .collect();
        defaults.sort_by_key(|route| (route.metric, route.slot));
        routes.extend(defaults);
        routes
    }

    /// The slot whose interface holds `addr`.
    pub(super) fn slot_with_addr(&self, addr: [u8; 4]) -> Option<usize> {
        self.units()
            .find(|(_, unit)| unit.stack.state().addr == Some(addr))
            .map(|(slot, _)| slot)
    }
}

/// `addr` with the host bits cleared.
pub(crate) fn network_of(addr: [u8; 4], prefix_len: u8) -> [u8; 4] {
    let mask = u32::MAX
        .checked_shl(32 - u32::from(prefix_len))
        .unwrap_or(0);
    (u32::from_be_bytes(addr) & mask).to_be_bytes()
}

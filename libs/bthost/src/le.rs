//! LE connections: which radio activity the host keeps running (the
//! accept-list auto-connect, a scan, one direct connect), which advertisers
//! it connects to, and what a fresh LE link starts with
//! (docs/bluetooth-plan.md B4, decisions 5 and 6).
//!
//! One thing at a time on the radio. The initiator and the scanner never run
//! together: a controller may refuse the combination, and nothing needs it.
//!
//! * **Bonded devices only** (no pairing window, no resolvable private
//!   address): the accept-list auto-connect `bthci::Init` armed in the fast
//!   path connects them, and is re-armed after each connection.
//! * **A bonded device with an IRK** changes address, and the controller has
//!   no resolving list here, so the host scans and resolves each connectable
//!   advertisement itself (`btsmp::func::rpa_matches`); the match is
//!   connected directly.
//! * **Pairing** (a window is open and the policy names LE): the same scan,
//!   for advertisers of the HID service or a HID appearance at an address the
//!   policy allows; the first match is connected directly.
//!
//! An advertiser that is neither bonded nor allowed is never connected, so an
//! unbonded LE device cannot even reach SMP while pairing is off.

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;

use btbond::{AddrKind, Bond};
use bthci::event::{AdvReport, LeConnection};
use bthci::opcode::status;
use bthci::{BdAddr, Command};
use btl2cap::LinkKind;
use btsmp::func::rpa_matches;

use crate::host::{red, Host};
use crate::le_link::{is_hid_adv, LeLink};
use crate::link::{Link, MAX_LINKS};

/// A direct connect attempt that finds nothing is cancelled after this long.
pub const DIRECT_MS: u64 = 5_000;
/// A refused radio command is not retried sooner than this.
pub const RETRY_MS: u64 = 1_000;
/// A link that was asked to encrypt must be encrypted within this long.
pub const ENCRYPT_MS: u64 = 10_000;
/// A pairing link gets this long to finish (the user types a passkey).
pub const PAIRING_SETUP_MS: u64 = 60_000;
/// Addresses refused or failed in one window; a device in pairing mode that
/// was refused keeps advertising and must not be connected in a loop.
const MAX_DENIED: usize = 8;

/// The scan the host runs: passive, 60 ms interval, 30 ms window.
const SCAN_INTERVAL: u16 = 0x0060;
const SCAN_WINDOW: u16 = 0x0030;
/// Advertising event types that accept a connection (ADV_IND, ADV_DIRECT_IND).
const ADV_IND: u8 = 0;
const ADV_DIRECT_IND: u8 = 1;

/// What the controller's initiator is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Initiator {
    Idle,
    /// `LE_Create_Connection` with the accept-list policy is pending.
    Auto,
    /// A connect to one address is pending.
    Direct,
    /// A cancel was sent; the failed connection complete ends it.
    Cancelling,
}

pub(crate) struct LeState {
    pub init: Initiator,
    pub scanning: bool,
    /// An advertiser to connect to next: `(address type, address)`.
    pub target: Option<(u8, BdAddr)>,
    /// The direct connect in flight, with its deadline.
    pub direct: Option<(u8, BdAddr, u64)>,
    pub retry_at: Option<u64>,
    pub denied: Vec<BdAddr>,
}

impl LeState {
    pub fn new() -> LeState {
        LeState {
            init: Initiator::Idle,
            scanning: false,
            target: None,
            direct: None,
            retry_at: None,
            denied: Vec::new(),
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        [self.direct.map(|d| d.2), self.retry_at]
            .into_iter()
            .flatten()
            .min()
    }
}

/// Whether `addr` is a resolvable private address (random, top bits `01`).
fn is_rpa(addr: BdAddr) -> bool {
    addr.0[5] & 0xC0 == 0x40
}

impl Host {
    fn le_ready(&self) -> bool {
        self.init.is_armed() && self.init.info().le_supported
    }

    /// The LE bond that `(address type, address)` is: the same identity, or
    /// a resolvable private address made from a bond's IRK.
    pub(crate) fn le_bond_for(&self, addr_type: u8, addr: BdAddr) -> Option<&Bond> {
        let kind = if addr_type & 1 == 0 {
            AddrKind::LePublic
        } else {
            AddrKind::LeRandom
        };
        if let Some(bond) = self.store.find(addr, kind) {
            return Some(bond);
        }
        if kind != AddrKind::LeRandom || !is_rpa(addr) {
            return None;
        }
        self.store
            .iter()
            .find(|b| b.kind.is_le() && b.irk.is_some_and(|irk| rpa_matches(&irk, &addr.0)))
    }

    fn le_linked(&self, addr: BdAddr) -> bool {
        self.links.iter().any(|l| l.addr == addr)
    }

    fn le_unlinked_bonds(&self) -> bool {
        self.store
            .iter()
            .any(|b| b.kind.is_le() && !self.le_linked(b.addr))
    }

    fn le_irk_bonds_unlinked(&self) -> bool {
        self.store
            .iter()
            .any(|b| b.kind.is_le() && b.irk.is_some() && !self.le_linked(b.addr))
    }

    /// Pairing over LE is on: a window is open and the policy names LE.
    pub(crate) fn le_pairing_active(&self, now: u64) -> bool {
        self.window_open(now)
            && (self.cfg.pair == crate::config::PairPolicy::Any || !self.cfg.le_targets.is_empty())
    }

    /// Whether `addr` may pair over LE now.
    pub(crate) fn le_pairing_allowed(&self, now: u64, addr: BdAddr) -> bool {
        self.window_open(now)
            && !self.le.denied.contains(&addr)
            && match self.cfg.pair {
                crate::config::PairPolicy::Any => true,
                _ => self.cfg.le_targets.contains(&addr),
            }
    }

    /// Do not connect to `addr` again for pairing in this window.
    pub(crate) fn le_deny(&mut self, addr: BdAddr) {
        if self.le.denied.len() < MAX_DENIED && !self.le.denied.contains(&addr) {
            self.le.denied.push(addr);
        }
    }

    // ---- the radio ---------------------------------------------------------

    /// The fast path is done: the auto-connect `Init` sent is in force.
    pub(crate) fn le_armed(&mut self, now: u64) {
        self.le_pump(now);
    }

    /// Decide the next radio command from what is wanted (see the module
    /// documentation). Called after anything that changes what is wanted or
    /// what the radio is doing.
    pub(crate) fn le_pump(&mut self, now: u64) {
        if !self.le_ready() || self.le.retry_at.is_some_and(|t| now < t) {
            return;
        }
        self.le.retry_at = None;
        // A direct connect in flight is left alone until it ends or its
        // deadline cancels it.
        if self.le.init == Initiator::Direct && self.le.target.is_none() {
            return;
        }
        let scan = self.le_pairing_active(now) || self.le_irk_bonds_unlinked();
        if let Some((ty, addr)) = self.le.target {
            return self.le_connect_target(now, ty, addr);
        }
        if scan {
            return self.le_start_scan();
        }
        if self.le.scanning {
            self.le.scanning = false;
            self.cmd(Command::LeSetScanEnable {
                enable: false,
                filter_duplicates: false,
            });
        }
        if self.le.init == Initiator::Idle && self.le_unlinked_bonds() {
            self.le.init = Initiator::Auto;
            self.cmd(bthci::init::le_auto_connect());
        }
    }

    fn le_cancel_initiator(&mut self) {
        self.le.init = Initiator::Cancelling;
        self.le.direct = None;
        self.cmd(Command::LeCreateConnectionCancel);
    }

    fn le_start_scan(&mut self) {
        match self.le.init {
            Initiator::Auto | Initiator::Direct => return self.le_cancel_initiator(),
            Initiator::Cancelling => return,
            Initiator::Idle => {}
        }
        if !self.le.scanning {
            self.le.scanning = true;
            self.cmd(Command::LeSetScanParameters {
                active: false,
                interval: SCAN_INTERVAL,
                window: SCAN_WINDOW,
                own_address_type: 0,
                filter_policy: 0,
            });
            self.cmd(Command::LeSetScanEnable {
                enable: true,
                filter_duplicates: false,
            });
        }
    }

    fn le_connect_target(&mut self, now: u64, ty: u8, addr: BdAddr) {
        if self.le.scanning {
            self.le.scanning = false;
            self.cmd(Command::LeSetScanEnable {
                enable: false,
                filter_duplicates: false,
            });
        }
        match self.le.init {
            Initiator::Idle => {
                self.le.target = None;
                self.le.init = Initiator::Direct;
                self.le.direct = Some((ty, addr, now.saturating_add(DIRECT_MS)));
                let mut create = bthci::init::le_auto_connect();
                if let Command::LeCreateConnection {
                    filter_policy,
                    peer_address_type,
                    peer,
                    ..
                } = &mut create
                {
                    *filter_policy = 0;
                    *peer_address_type = ty & 1;
                    *peer = addr;
                }
                self.cmd(create);
            }
            Initiator::Auto | Initiator::Direct => self.le_cancel_initiator(),
            Initiator::Cancelling => {}
        }
    }

    /// An advertisement while scanning: connect to a bonded device (known
    /// address or one its IRK makes), or to a pairing candidate.
    pub(crate) fn le_on_adv(&mut self, now: u64, report: &AdvReport) {
        if !self.le.scanning
            || self.le.target.is_some()
            || !matches!(report.event_type, ADV_IND | ADV_DIRECT_IND)
            || report.address_type > 1
            || self.links.len() >= MAX_LINKS
        {
            return;
        }
        let (ty, addr) = (report.address_type, report.addr);
        let wanted = match self.le_bond_for(ty, addr) {
            Some(bond) => !self.le_linked(bond.addr),
            None => {
                !self.le_linked(addr)
                    && is_hid_adv(&report.data)
                    && self.le_pairing_allowed(now, addr)
            }
        };
        if wanted {
            self.le.target = Some((ty, addr));
        }
    }

    /// A command the controller refused: a refused initiator is idle.
    pub(crate) fn le_command_refused(&mut self, now: u64, op: u16) {
        if op == bthci::opcode::LE_CREATE_CONNECTION {
            self.le.init = Initiator::Idle;
            self.le.direct = None;
            self.le.retry_at = Some(now.saturating_add(RETRY_MS));
        }
    }

    /// `LE_Create_Connection` was accepted by the controller (by `Init`'s
    /// fast path, or by us): the initiator is busy.
    pub(crate) fn le_create_accepted(&mut self) {
        if self.le.init == Initiator::Idle {
            self.le.init = Initiator::Auto;
        }
    }

    /// The cancel found nothing to cancel.
    pub(crate) fn le_cancel_result(&mut self, now: u64, code: u8) {
        if code != status::SUCCESS && self.le.init == Initiator::Cancelling {
            self.le.init = Initiator::Idle;
            self.le_pump(now);
        }
    }

    /// Deadlines of the radio: a direct connect that finds nothing, a retry.
    pub(crate) fn le_tick(&mut self, now: u64) {
        if let Some((_, addr, due)) = self.le.direct {
            if now >= due {
                self.le_deny(addr);
                self.le_cancel_initiator();
            }
        }
        self.le_pump(now);
    }

    // ---- connections -------------------------------------------------------

    pub(crate) fn on_le_connection(&mut self, now: u64, c: LeConnection) {
        let was = self.le.init;
        self.le.init = Initiator::Idle;
        let direct = self.le.direct.take();
        if c.status != status::SUCCESS {
            // A cancelled or failed initiator; a failed direct attempt is
            // not retried in this window.
            if was == Initiator::Direct {
                if let Some((_, addr, _)) = direct {
                    self.le_deny(addr);
                }
                self.note(format!("CONN:FAIL addr=le status={:#04x}", c.status));
            }
            return self.le_pump(now);
        }
        self.le_accept_link(now, &c);
        self.le_pump(now);
    }

    fn le_accept_link(&mut self, now: u64, c: &LeConnection) {
        let addr_type = c.peer_address_type & 1;
        let bad = c.role != 0
            || self.link_idx(c.handle).is_some()
            || self.links.len() >= MAX_LINKS
            || self.l2.link_up(c.handle, LinkKind::Le).is_err();
        if bad {
            return self.cmd(Command::Disconnect {
                handle: c.handle,
                reason: status::CONNECTION_TERMINATED_BY_LOCAL_HOST,
            });
        }
        let bond = self
            .le_bond_for(addr_type, c.peer)
            .map(|b| (b.addr, b.ltk.clone()));
        let identity = bond.as_ref().map_or(c.peer, |b| b.0);
        let pairing = self.le_pairing_allowed(now, identity);
        if bond.is_none() && !pairing {
            // Decision 6: an unbonded device connects only to pair.
            self.refuse("unbonded", c.peer);
            return self.le_drop(c.handle);
        }
        if self.link_idx_by_addr(identity).is_some() {
            self.refuse("links", identity);
            return self.le_drop(c.handle);
        }
        let mut link = Link::new(c.handle, identity, now);
        link.reconnect = bond.is_some();
        link.pairing = pairing;
        link.le = Some(Box::new(LeLink::new(addr_type, c.peer)));
        if pairing {
            link.setup_deadline = now.saturating_add(PAIRING_SETUP_MS);
        }
        // `rpa=1`: the device came under a resolvable private address that
        // its IRK resolved to the bond (a fresh address each connection).
        self.note(format!(
            "CONN addr={} handle={:#06x} role=le rpa={}",
            red(identity),
            c.handle,
            u8::from(identity != c.peer)
        ));
        self.links.push(link);
        match bond {
            Some((_, Some(ltk))) => {
                self.le_encrypt_with(now, c.handle, ltk.key, ltk.ediv, ltk.rand)
            }
            Some((_, None)) => self.le_drop(c.handle),
            None => self.le_begin_pairing(now, c.handle),
        }
    }

    /// Release a connection that never became a link.
    fn le_drop(&mut self, handle: u16) {
        let _ = self.l2.link_down(handle);
        self.cmd(Command::Disconnect {
            handle,
            reason: status::CONNECTION_TERMINATED_BY_LOCAL_HOST,
        });
    }

    pub(crate) fn set_replaying(&mut self, handle: u16, on: bool) {
        let le = self
            .link_idx(handle)
            .and_then(|i| self.links[i].le.as_mut());
        if let Some(le) = le {
            le.replaying = on;
        }
    }

    /// An LE link ended: the radio may have something new to do.
    pub(crate) fn le_link_down(&mut self, now: u64) {
        self.le_pump(now);
    }
}

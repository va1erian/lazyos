//! Pages the host sends itself (docs/bluetooth-plan.md, Risk 2): a bonded
//! BR/EDR device that does not page the host (`HIDReconnectInitiate` false,
//! `HIDNormallyConnectable` true) waits to be paged, and a device in the
//! pairing list is paged to pair with. One page at a time, a bounded number
//! of attempts each: a mouse in a drawer must not keep the radio busy.

use alloc::format;
use alloc::vec::Vec;

use btbond::bond::hid_flag;
use btbond::AddrKind;
use bthci::opcode;
use bthci::{BdAddr, Command};

use crate::config::PairPolicy;
use crate::host::{red, Host};

/// Attempts at a bonded device after boot.
pub const BOOT_ATTEMPTS: u8 = 5;
/// Attempts after a link to it was lost.
pub const RELINK_ATTEMPTS: u8 = 3;
/// Attempts at a pairing target.
pub const PAIR_ATTEMPTS: u8 = 3;
/// Pause between attempts.
pub const RETRY_MS: u64 = 1_500;
/// The controller's page timeout is about 5 s; a page still unanswered after
/// this long is counted as failed and the next one may start.
pub const IN_FLIGHT_MS: u64 = 15_000;
/// Parameters of `Create_Connection`: DM1/DH1/DM3/DH3/DM5/DH5, mandatory page
/// scan repetition mode R2, role switch allowed.
const PACKET_TYPES: u16 = 0xCC18;
const SCAN_REPETITION_R2: u8 = 2;

#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub addr: BdAddr,
    /// A pairing target rather than a reconnect.
    pub pairing: bool,
    pub attempts_left: u8,
    pub next_at: u64,
}

impl Job {
    /// A page at a device the caller is pairing with.
    pub(crate) fn pairing(addr: BdAddr, attempts: u8, now: u64) -> Job {
        Job {
            addr,
            pairing: true,
            attempts_left: attempts,
            next_at: now,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Pager {
    jobs: Vec<Job>,
    /// The page in flight: address, whether it pairs, its deadline.
    in_flight: Option<(BdAddr, bool, u64)>,
}

impl Pager {
    /// When the pager next needs the host to look.
    pub fn deadline(&self) -> Option<u64> {
        match self.in_flight {
            Some((_, _, deadline)) => Some(deadline),
            None => self.jobs.iter().map(|j| j.next_at).min(),
        }
    }

    pub(crate) fn add(&mut self, job: Job) {
        if !self.jobs.iter().any(|j| j.addr == job.addr) {
            self.jobs.push(job);
        }
    }

    fn drop_job(&mut self, addr: BdAddr) {
        self.jobs.retain(|j| j.addr != addr);
    }

    /// The page for `addr` is over (connected or failed); whether it was a
    /// pairing page, if it was ours.
    pub fn finish(&mut self, addr: BdAddr) -> Option<bool> {
        match self.in_flight {
            Some((a, pairing, _)) if a == addr => {
                self.in_flight = None;
                Some(pairing)
            }
            _ => None,
        }
    }
}

impl Host {
    /// When the pager needs the host to look. While an inquiry or a name
    /// request owns the radio a due job cannot start and nothing is due: the
    /// event that ends it wakes the host (a past deadline would spin).
    pub(crate) fn pager_deadline(&self) -> Option<u64> {
        if self.pager.in_flight.is_none() && self.ctl_radio_busy() {
            return None;
        }
        self.pager.deadline()
    }

    /// The fast path is done: queue the pages that wait for the host.
    pub(crate) fn pager_arm(&mut self, now: u64) {
        let waiting: Vec<BdAddr> = self
            .store
            .iter()
            .filter(|b| {
                b.kind == AddrKind::BrEdr
                    && !b.report_descriptor.is_empty()
                    && b.hid_flags & hid_flag::RECONNECT_INITIATE == 0
            })
            .map(|b| b.addr)
            .collect();
        for addr in waiting.into_iter().filter(|_| !self.key_size_unsupported) {
            self.pager.add(Job {
                addr,
                pairing: false,
                attempts_left: BOOT_ATTEMPTS,
                next_at: now,
            });
        }
        if let PairPolicy::Targets(targets) = self.cfg.pair.clone() {
            for addr in targets {
                if !self.is_bonded(addr) {
                    self.pager.add(Job {
                        addr,
                        pairing: true,
                        attempts_left: PAIR_ATTEMPTS,
                        next_at: now,
                    });
                }
            }
        }
    }

    /// A link to a device that waits for the host was lost: try again, a
    /// bounded number of times.
    pub(crate) fn pager_relink(&mut self, now: u64, addr: BdAddr) {
        let waits = self.store.find(addr, AddrKind::BrEdr).is_some_and(|b| {
            !b.report_descriptor.is_empty() && b.hid_flags & hid_flag::RECONNECT_INITIATE == 0
        });
        if waits && self.init.is_armed() && !self.key_size_unsupported {
            self.pager.add(Job {
                addr,
                pairing: false,
                attempts_left: RELINK_ATTEMPTS,
                next_at: now.saturating_add(RETRY_MS),
            });
        }
    }

    /// Start the next page if the radio is free and a job is due.
    pub(crate) fn pager_tick(&mut self, now: u64) {
        if let Some((addr, pairing, deadline)) = self.pager.in_flight {
            if now >= deadline {
                self.pager.in_flight = None;
                self.page_failed(now, addr, pairing);
            }
            return;
        }
        // An inquiry or a name request owns the radio until it ends.
        if !self.init.is_armed() || self.ctl_radio_busy() {
            return;
        }
        // A job whose device has connected by itself is done.
        let linked: Vec<BdAddr> = self.links.iter().map(|l| l.addr).collect();
        // A controller that cannot tell the key size refuses every reconnect
        // (`knob.rs`): paging those devices again would only repeat that.
        let no_reconnects = self.key_size_unsupported;
        self.pager
            .jobs
            .retain(|j| !linked.contains(&j.addr) && (j.pairing || !no_reconnects));
        let due = self.pager.jobs.iter().position(|j| j.next_at <= now);
        let Some(index) = due else { return };
        let job = self.pager.jobs[index].clone();
        self.cmd(Command::CreateConnection {
            addr: job.addr,
            packet_type: PACKET_TYPES,
            page_scan_repetition_mode: SCAN_REPETITION_R2,
            clock_offset: 0,
            allow_role_switch: 1,
        });
        self.pager.in_flight = Some((job.addr, job.pairing, now.saturating_add(IN_FLIGHT_MS)));
    }

    /// A page ended without a link: count the attempt, retry or give up.
    pub(crate) fn page_failed(&mut self, now: u64, addr: BdAddr, pairing: bool) {
        let Some(job) = self.pager.jobs.iter_mut().find(|j| j.addr == addr) else {
            return;
        };
        job.attempts_left = job.attempts_left.saturating_sub(1);
        job.next_at = now.saturating_add(RETRY_MS);
        if job.attempts_left == 0 {
            self.pager.drop_job(addr);
            let what = if pairing { "pair" } else { "reconnect" };
            self.note(format!("RECONNECT:GIVEUP addr={} for={what}", red(addr)));
            self.ctl_failed(addr, crate::ctl_types::FailReason::Timeout);
        }
    }

    /// The controller refused a command; a refused `Create_Connection` is a
    /// failed page.
    pub(crate) fn on_command_refused(&mut self, now: u64, op: u16, code: u8) {
        self.le_command_refused(now, op);
        self.ctl_command_refused(now, op);
        if op == opcode::READ_ENCRYPTION_KEY_SIZE {
            self.key_size_unavailable(code);
        }
        if op != opcode::CREATE_CONNECTION {
            return;
        }
        if let Some((addr, pairing, _)) = self.pager.in_flight.take() {
            self.note(format!("CONN:FAIL addr={} status={code:#04x}", red(addr)));
            self.page_failed(now, addr, pairing);
        }
    }

    /// A link came up for `addr`: the page (if it was ours) succeeded.
    pub(crate) fn page_connected(&mut self, addr: BdAddr) -> Option<bool> {
        let pairing = self.pager.finish(addr);
        if pairing.is_some() {
            self.pager.drop_job(addr);
        }
        pairing
    }

    /// A device was forgotten: stop paging it.
    pub(crate) fn pager_forget(&mut self, addr: BdAddr) {
        self.pager.drop_job(addr);
    }

    /// A page ended with a failure status from the controller.
    pub(crate) fn page_status_failed(&mut self, now: u64, addr: BdAddr) {
        if let Some(pairing) = self.pager.finish(addr) {
            self.page_failed(now, addr, pairing);
        }
    }
}

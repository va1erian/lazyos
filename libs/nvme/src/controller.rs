//! Controller bring-up, polled I/O and shutdown (NVMe 1.4 section 7.6).
//!
//! [`Controller::init`] resets the controller (`CC.EN = 0`, waiting for
//! `CSTS.RDY = 0` bounded by `CAP.TO`), sets up the admin queue pair,
//! enables it, reads Identify Controller and Identify Namespace 1, and
//! creates one I/O completion queue and one I/O submission queue. Commands
//! carry no interrupt: the host polls the completion queue's phase bit.
//!
//! [`Controller::transfer`] cuts a read or write into commands
//! ([`crate::prp`]), keeps up to [`MAX_INFLIGHT`] of them in the queue, and
//! returns only once every command it submitted completed. Waiting is the
//! caller's (`wait`): the kernel parks or spins there. A wait that gives up
//! disables the controller, which then touches no memory, and the controller
//! is detached for good.

use crate::cmd::{cns, nvm, Command, Completion};
use crate::identify::{ControllerInfo, Namespace, NamespaceError, PAGE_BYTES};
use crate::prp::{self, Cursor, PlanError, MAX_ENTRIES};
use crate::queue::QueuePair;
use crate::regs::{self, cc, csts, Cap};
use crate::{Error, Platform};

/// Commands of one transfer in the I/O queue at once.
pub const MAX_INFLIGHT: usize = 8;
/// Admin queue entries.
pub const ADMIN_DEPTH: u16 = 16;
/// I/O queue entries (fewer when `CAP.MQES` says so).
pub const IO_DEPTH: u16 = 64;
/// Bytes in one command at most (fewer when `MDTS` says so).
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// The namespace this driver serves.
pub const NSID: u32 = 1;
/// How long an admin command may take.
const ADMIN_TIMEOUT_NS: u64 = 5_000_000_000;
/// How long the shutdown notification may take before it is abandoned.
const SHUTDOWN_TIMEOUT_NS: u64 = 5_000_000_000;
/// Completions naming no command of ours, in a row, before the controller
/// is declared confused and detached.
const STRAY_LIMIT: usize = 256;
/// Polls before a wait gives up even if the clock never moves.
const POLL_BACKSTOP: u64 = 50_000_000;

/// Physical pages the caller provides, each 4 KiB, page aligned, zeroed and
/// owned by the controller for as long as it is attached.
#[derive(Clone, Copy, Debug)]
pub struct Pages {
    pub admin_sq: u64,
    pub admin_cq: u64,
    pub io_sq: u64,
    pub io_cq: u64,
    /// Identify data lands here.
    pub identify: u64,
    /// One PRP list page per in-flight command.
    pub prp_lists: [u64; MAX_INFLIGHT],
}

/// Read or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

/// One command of a transfer in the queue.
#[derive(Clone, Copy, Debug)]
struct Inflight {
    cid: u16,
}

/// A brought-up controller and its namespace.
pub struct Controller {
    pub cap: Cap,
    pub info: ControllerInfo,
    pub namespace: Namespace,
    pages: Pages,
    admin: QueuePair,
    io: QueuePair,
    /// Bytes in one command (a block multiple).
    max_command: usize,
    /// Rolls command identifiers so a stale completion never matches.
    sequence: u16,
    detached: bool,
}

/// Poll `ready` until it holds or `timeout_ns` passes.
fn poll(platform: &dyn Platform, timeout_ns: u64, mut ready: impl FnMut() -> bool) -> bool {
    let start = platform.now_ns();
    for _ in 0..POLL_BACKSTOP {
        if ready() {
            return true;
        }
        if platform.now_ns().saturating_sub(start) >= timeout_ns {
            return ready();
        }
        platform.relax();
    }
    ready()
}

/// Clear `CC.EN` and wait for `CSTS.RDY` to drop. A controller that is
/// still coming up from an earlier enable is let finish first (the spec
/// forbids clearing `EN` while `RDY` lags it).
fn disable(platform: &dyn Platform, cap: &Cap) -> Result<(), Error> {
    let timeout = cap.timeout_ms * 1_000_000;
    let config = platform.read32(regs::CC);
    if config & cc::EN != 0 {
        let settled = |platform: &dyn Platform| {
            let status = platform.read32(regs::CSTS);
            status & csts::RDY != 0 || status & csts::CFS != 0
        };
        poll(platform, timeout, || settled(platform));
        platform.write32(regs::CC, config & !cc::EN);
    }
    if poll(platform, timeout, || {
        platform.read32(regs::CSTS) & csts::RDY == 0
    }) {
        Ok(())
    } else {
        Err(Error::Timeout)
    }
}

impl Controller {
    /// Bring the controller up and identify namespace [`NSID`].
    pub fn init(platform: &dyn Platform, pages: Pages) -> Result<Controller, Error> {
        let cap = Cap::decode(regs::read64(platform, regs::CAP));
        if !cap.nvm {
            return Err(Error::Unsupported("no NVM command set"));
        }
        if cap.page_min_shift > 12 {
            return Err(Error::Unsupported("4 KiB memory pages not supported"));
        }
        if cap.mqes < 2 {
            return Err(Error::Unsupported("queues too small"));
        }
        disable(platform, &cap)?;
        // Mask every interrupt vector: the driver polls.
        platform.write32(regs::INTMS, u32::MAX);

        let admin_depth = ADMIN_DEPTH.min(cap.mqes.min(u32::from(u16::MAX)) as u16);
        let admin = QueuePair::new(
            0,
            admin_depth,
            pages.admin_sq,
            pages.admin_cq,
            cap.doorbell_stride,
        );
        platform.write32(regs::AQA, regs::aqa(admin_depth, admin_depth));
        regs::write64(platform, regs::ASQ, pages.admin_sq);
        regs::write64(platform, regs::ACQ, pages.admin_cq);
        platform.write32(regs::CC, regs::cc_enable());
        let timeout = cap.timeout_ms * 1_000_000;
        let up = poll(platform, timeout, || {
            platform.read32(regs::CSTS) & (csts::RDY | csts::CFS) != 0
        });
        let status = platform.read32(regs::CSTS);
        if status & csts::CFS != 0 {
            return Err(Error::Fatal);
        }
        if !up || status & csts::RDY == 0 {
            // Leave it disabled rather than half enabled.
            platform.write32(regs::CC, 0);
            return Err(Error::Timeout);
        }

        let io_depth = IO_DEPTH.min(cap.mqes.min(u32::from(u16::MAX)) as u16);
        let mut controller = Controller {
            cap,
            info: ControllerInfo::parse(&[0; PAGE_BYTES]).ok_or(Error::Fatal)?,
            namespace: Namespace {
                blocks: 0,
                block_bytes: 512,
                format: 0,
            },
            pages,
            admin,
            io: QueuePair::new(1, io_depth, pages.io_sq, pages.io_cq, cap.doorbell_stride),
            max_command: MAX_COMMAND_BYTES,
            sequence: 0,
            detached: false,
        };
        match controller.bring_up(platform) {
            Ok(()) => Ok(controller),
            Err(error) => {
                controller.detach(platform);
                Err(error)
            }
        }
    }

    /// Identify, then create the I/O queue pair.
    fn bring_up(&mut self, platform: &dyn Platform) -> Result<(), Error> {
        let mut page = [0u8; PAGE_BYTES];
        self.admin(
            platform,
            Command::identify(cns::CONTROLLER, 0, self.pages.identify),
        )?;
        platform.read_mem(self.pages.identify, &mut page);
        self.info = ControllerInfo::parse(&page).ok_or(Error::Fatal)?;
        if self.info.sqes_min > 6 || self.info.cqes_min > 4 {
            return Err(Error::Unsupported("larger queue entries required"));
        }
        if self.info.namespaces < NSID {
            return Err(Error::Unsupported("no namespace 1"));
        }

        page.fill(0);
        platform.write_mem(self.pages.identify, &page);
        self.admin(
            platform,
            Command::identify(cns::NAMESPACE, NSID, self.pages.identify),
        )?;
        platform.read_mem(self.pages.identify, &mut page);
        self.namespace = Namespace::parse(&page).map_err(|error| {
            Error::Unsupported(match error {
                NamespaceError::Empty => "namespace 1 inactive",
                NamespaceError::BadFormat => "namespace format index out of range",
                NamespaceError::BadBlockSize => "namespace block size out of range",
                NamespaceError::Metadata => "namespace format carries metadata",
                NamespaceError::Inconsistent => "namespace sizes inconsistent",
            })
        })?;
        let block = self.namespace.block_bytes as usize;
        let limit = self
            .info
            .max_transfer(self.cap.page_min_shift)
            .map_or(MAX_COMMAND_BYTES, |bytes| {
                bytes.min(MAX_COMMAND_BYTES as u64) as usize
            });
        self.max_command = limit - limit % block;
        if self.max_command == 0 {
            return Err(Error::Unsupported("MDTS below one block"));
        }

        // Ask for one queue of each kind. Controllers must accept it; a
        // refusal is not fatal, since queue 1 always exists.
        let _ = self.admin(platform, Command::set_queue_count(1));
        let depth = self.io.depth;
        self.admin(platform, Command::create_cq(1, depth, self.pages.io_cq))?;
        self.admin(platform, Command::create_sq(1, depth, self.pages.io_sq, 1))?;
        Ok(())
    }

    /// Run one admin command to completion.
    fn admin(
        &mut self,
        platform: &dyn Platform,
        mut command: Command,
    ) -> Result<Completion, Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        command.cid = self.next_cid(0);
        self.admin.submit(platform, &command);
        let admin = &self.admin;
        if !poll(platform, ADMIN_TIMEOUT_NS, || admin.ready(platform)) {
            return Err(Error::Timeout);
        }
        let status = platform.read32(regs::CSTS);
        if status & csts::CFS != 0 {
            return Err(Error::Fatal);
        }
        // Admin commands run one at a time: the head entry is ours unless the
        // controller is confused, which is fatal for bring-up.
        let entry = self.admin.pop(platform).ok_or(Error::Timeout)?;
        if entry.cid != command.cid {
            return Err(Error::Fatal);
        }
        if !entry.ok() {
            return Err(Error::Status {
                sct: entry.sct,
                sc: entry.sc,
            });
        }
        Ok(entry)
    }

    /// A fresh command identifier for in-flight slot `slot`.
    fn next_cid(&mut self, slot: usize) -> u16 {
        self.sequence = self.sequence.wrapping_add(1) & 0x1FFF;
        self.sequence << 3 | slot as u16
    }

    /// Bytes in one command.
    pub fn max_command_bytes(&self) -> usize {
        self.max_command
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }

    /// Disable the controller so it stops touching memory, and refuse every
    /// later request.
    pub fn detach(&mut self, platform: &dyn Platform) {
        self.detached = true;
        let _ = disable(platform, &self.cap);
    }

    /// Move the bytes of `segments` (`(virtual address, length)`, back to
    /// back) to (`Op::Write`) or from the namespace at block `lba`.
    /// `translate` maps a virtual address to its physical one; `wait` is
    /// called with a readiness test while commands are outstanding and
    /// returns whether it held before the caller's deadline.
    pub fn transfer(
        &mut self,
        platform: &dyn Platform,
        op: Op,
        lba: u64,
        segments: &[(u64, usize)],
        translate: &dyn Fn(u64) -> Option<u64>,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        let block = self.namespace.block_bytes as usize;
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        if !total.is_multiple_of(block) {
            return Err(Error::Bounds);
        }
        let blocks = (total / block) as u64;
        if lba
            .checked_add(blocks)
            .is_none_or(|end| end > self.namespace.blocks)
        {
            return Err(Error::Bounds);
        }
        let opcode = match op {
            Op::Read => nvm::READ,
            Op::Write => nvm::WRITE,
        };
        let mut cursor = Cursor::default();
        cursor.advance(segments, 0);
        let mut submitted = 0usize;
        let mut slots: [Option<Inflight>; MAX_INFLIGHT] = [None; MAX_INFLIGHT];
        let mut result = Ok(());
        let mut strays = 0usize;
        loop {
            // Submit what fits.
            while result.is_ok() && submitted < total {
                let Some(slot) = slots.iter().position(Option::is_none) else {
                    break;
                };
                let plan = match prp::plan(segments, cursor, self.max_command, block, translate) {
                    Ok(plan) => plan,
                    Err(PlanError::Unmapped) => {
                        result = Err(Error::Unmapped);
                        break;
                    }
                    Err(PlanError::Misaligned) => {
                        result = Err(Error::Misaligned);
                        break;
                    }
                };
                let prp2 = if plan.needs_list() {
                    let mut list = [0u8; MAX_ENTRIES * 8];
                    let len = plan.list(&mut list);
                    platform.write_mem(self.pages.prp_lists[slot], &list[..len]);
                    self.pages.prp_lists[slot]
                } else {
                    plan.prp2_direct()
                };
                let at = lba + (submitted / block) as u64;
                let mut command = Command::io(
                    opcode,
                    NSID,
                    at,
                    (plan.bytes / block) as u32,
                    plan.prp1(),
                    prp2,
                );
                command.cid = self.next_cid(slot);
                self.io.submit(platform, &command);
                slots[slot] = Some(Inflight { cid: command.cid });
                cursor.advance(segments, plan.bytes);
                submitted += plan.bytes;
            }
            // Reap what finished (at most a queue's worth per round).
            for _ in 0..self.io.depth {
                let Some(entry) = self.io.pop(platform) else {
                    break;
                };
                let slot = usize::from(entry.cid & 0x7);
                match slots[slot] {
                    Some(inflight) if inflight.cid == entry.cid => {
                        if !entry.ok() && result.is_ok() {
                            result = Err(Error::Status {
                                sct: entry.sct,
                                sc: entry.sc,
                            });
                        }
                        slots[slot] = None;
                        strays = 0;
                    }
                    // Not a command of ours in flight: a confused controller.
                    _ => strays += 1,
                }
            }
            if strays > STRAY_LIMIT {
                self.detach(platform);
                return Err(Error::Fatal);
            }
            let outstanding = slots.iter().any(Option::is_some);
            if !outstanding && (submitted == total || result.is_err()) {
                return result;
            }
            if !outstanding {
                continue;
            }
            let io = &self.io;
            let ready = || io.ready(platform) || platform.read32(regs::CSTS) & csts::CFS != 0;
            if !wait(&ready) {
                self.detach(platform);
                return Err(Error::Timeout);
            }
            if platform.read32(regs::CSTS) & csts::CFS != 0 {
                self.detach(platform);
                return Err(Error::Fatal);
            }
        }
    }

    /// Flush the volatile write cache, if the controller has one.
    pub fn flush(
        &mut self,
        platform: &dyn Platform,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        if !self.info.volatile_cache {
            return Ok(());
        }
        let mut command = Command::flush(NSID);
        command.cid = self.next_cid(0);
        self.io.submit(platform, &command);
        let mut strays = 0usize;
        loop {
            while let Some(entry) = self.io.pop(platform) {
                if entry.cid == command.cid {
                    return if entry.ok() {
                        Ok(())
                    } else {
                        Err(Error::Status {
                            sct: entry.sct,
                            sc: entry.sc,
                        })
                    };
                }
                strays += 1;
                if strays > STRAY_LIMIT {
                    self.detach(platform);
                    return Err(Error::Fatal);
                }
            }
            let io = &self.io;
            if !wait(&|| io.ready(platform)) {
                self.detach(platform);
                return Err(Error::Timeout);
            }
        }
    }

    /// Normal shutdown notification: the controller writes its caches back
    /// and says when it is safe to cut power. Detaches the controller.
    pub fn shutdown(&mut self, platform: &dyn Platform) -> Result<(), Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        self.detached = true;
        let config = platform.read32(regs::CC);
        platform.write32(regs::CC, (config & !cc::SHN_MASK) | cc::SHN_NORMAL);
        let done = poll(platform, SHUTDOWN_TIMEOUT_NS, || {
            platform.read32(regs::CSTS) & csts::SHST_MASK == csts::SHST_COMPLETE
        });
        if done {
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }
}

//! Controller bring-up as a pure step function.
//!
//! [`Init::step`] takes the time and one input (start, an event from the
//! controller, or a clock tick) and returns the next command to send and the
//! phase. It does no I/O and reads no clock, so the same code runs in `btd`
//! and against the simulator.
//!
//! The script puts the **boot fast path** first (`docs/bluetooth-plan.md`
//! decision 5): everything that lets a bonded device in comes before anything
//! cosmetic.
//!
//! 1. `Reset`, then `Read_Buffer_Size` (an incoming link needs the buffers
//!    the moment it is up), the event masks, and Secure Simple Pairing on
//!    (cheap, and a peer that connects first and pairs a moment later needs
//!    it).
//! 2. BR/EDR **page scan** on, so a bonded keyboard that pages the host is
//!    answered.
//! 3. The LE **accept list** of bonded devices and an auto-connect
//!    (`LE_Create_Connection` with the accept-list filter policy).
//!
//! When step 3 completes the phase becomes [`Phase::SlowPath`] ("armed": a
//! bonded device can connect now). Then, in order: class of device, local
//! name, `Read_BD_ADDR`, `Read_Local_Version`. A controller without LE (the
//! LE probe fails) skips every LE command.

use alloc::vec;
use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::command::Command;
use crate::event::Event;
use crate::opcode::{self as op, status};
use crate::ret::{self, BufferSize, LocalVersion};

/// The most accept-list entries one init sends (controllers hold 8 to 100+;
/// more bonds than this are reconnected by scanning instead).
pub const MAX_ACCEPT_LIST: usize = 16;
pub const RESET_TIMEOUT_MS: u64 = 3_000;
pub const COMMAND_TIMEOUT_MS: u64 = 1_000;
/// We are a Computer (major class 0x01, minor Desktop 0x01), offering no
/// services.
pub const CLASS_COMPUTER: u32 = 0x0001 << 8 | 0x01 << 2;

#[derive(Clone, Debug)]
pub struct InitConfig {
    /// Turn BR/EDR page scan on in the fast path.
    pub page_scan: bool,
    /// Bonded LE devices (address type, address) for the accept list.
    pub le_accept_list: Vec<(u8, BdAddr)>,
    pub class_of_device: u32,
    pub local_name: Vec<u8>,
}

impl Default for InitConfig {
    fn default() -> Self {
        InitConfig {
            page_scan: true,
            le_accept_list: Vec::new(),
            class_of_device: CLASS_COMPUTER,
            local_name: b"LazyOS".to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    CommandFailed {
        opcode: u16,
        status: u8,
    },
    Timeout {
        opcode: u16,
    },
    /// A response too short to hold what the command returns.
    BadResponse {
        opcode: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    NotStarted,
    /// Reset through the LE auto-connect: not yet able to admit a device.
    FastPath,
    /// The fast path is done ("armed"); the rest is cosmetic.
    SlowPath,
    Complete,
    Failed(InitError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input<'a> {
    Start,
    Event(&'a Event),
    Tick,
}

/// What the caller does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Hand this to the command queue.
    pub send: Option<Command>,
    pub phase: Phase,
    /// Call again with [`Input::Tick`] at or after this time.
    pub deadline_ms: Option<u64>,
}

/// What the controller turned out to be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControllerInfo {
    pub bd_addr: Option<BdAddr>,
    pub version: Option<LocalVersion>,
    pub acl: BufferSize,
    /// LE buffers; `acl_len == 0` means LE shares [`ControllerInfo::acl`].
    pub le_acl: BufferSize,
    pub le_supported: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Failure aborts bring-up.
    Required,
    /// Failure is noted and skipped (not every controller has the feature).
    Optional,
    /// Failure means "no LE": drop the LE commands.
    LeProbe,
}

struct Item {
    cmd: Command,
    need: Need,
    le: bool,
    fast: bool,
}

pub struct Init {
    script: Vec<Item>,
    next: usize,
    pending: Option<(u16, u64)>,
    phase: Phase,
    reset_retried: bool,
    info: ControllerInfo,
}

fn item(cmd: Command, need: Need, le: bool, fast: bool) -> Item {
    Item {
        cmd,
        need,
        le,
        fast,
    }
}

fn build_script(cfg: &InitConfig) -> Vec<Item> {
    use Need::*;
    let mut s = vec![
        item(Command::Reset, Required, false, true),
        item(Command::ReadBufferSize, Required, false, true),
        item(
            Command::SetEventMask(op::EVENT_MASK_HID_HOST),
            Required,
            false,
            true,
        ),
        item(Command::LeReadBufferSize, LeProbe, true, true),
        item(
            Command::LeSetEventMask(op::LE_EVENT_MASK_HID_HOST),
            Optional,
            true,
            true,
        ),
        item(Command::WriteSimplePairingMode(true), Optional, false, true),
    ];
    if cfg.page_scan {
        s.push(item(Command::WriteScanEnable(0x02), Required, false, true));
    }
    let n = cfg.le_accept_list.len().min(MAX_ACCEPT_LIST);
    if n > 0 {
        s.push(item(Command::LeClearAcceptList, Optional, true, true));
        for (ty, addr) in &cfg.le_accept_list[..n] {
            let cmd = Command::LeAddDeviceToAcceptList {
                address_type: *ty,
                addr: *addr,
            };
            s.push(item(cmd, Optional, true, true));
        }
        s.push(item(le_auto_connect(), Optional, true, true));
    }
    s.push(item(
        Command::WriteClassOfDevice(cfg.class_of_device),
        Optional,
        false,
        false,
    ));
    s.push(item(
        Command::WriteLocalName(cfg.local_name.clone()),
        Optional,
        false,
        false,
    ));
    s.push(item(Command::ReadBdAddr, Optional, false, false));
    s.push(item(Command::ReadLocalVersion, Optional, false, false));
    s
}

/// Connect to any device on the accept list; intervals suit a HID peripheral
/// (30 to 50 ms, supervision timeout 4 s).
pub(crate) fn le_auto_connect() -> Command {
    Command::LeCreateConnection {
        scan_interval: 0x0060,
        scan_window: 0x0030,
        filter_policy: 1,
        peer_address_type: 0,
        peer: BdAddr::default(),
        own_address_type: 0,
        interval_min: 0x0018,
        interval_max: 0x0028,
        latency: 0,
        supervision_timeout: 0x0190,
        min_ce_length: 0,
        max_ce_length: 0,
    }
}

impl Init {
    pub fn new(cfg: &InitConfig) -> Init {
        Init {
            script: build_script(cfg),
            next: 0,
            pending: None,
            phase: Phase::NotStarted,
            reset_retried: false,
            info: ControllerInfo::default(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn info(&self) -> &ControllerInfo {
        &self.info
    }

    /// True once a bonded device can connect (fast path done).
    pub fn is_armed(&self) -> bool {
        matches!(self.phase, Phase::SlowPath | Phase::Complete)
    }

    pub fn step(&mut self, now_ms: u64, input: Input<'_>) -> Step {
        match (self.phase, input) {
            (Phase::NotStarted, Input::Start) => {
                self.phase = Phase::FastPath;
                self.send_next(now_ms)
            }
            (Phase::FastPath | Phase::SlowPath, Input::Event(ev)) => self.on_event(now_ms, ev),
            (Phase::FastPath | Phase::SlowPath, Input::Tick) => self.on_tick(now_ms),
            _ => self.idle_step(),
        }
    }

    fn idle_step(&self) -> Step {
        Step {
            send: None,
            phase: self.phase,
            deadline_ms: self.pending.map(|(_, d)| d),
        }
    }

    fn fail(&mut self, e: InitError) -> Step {
        self.phase = Phase::Failed(e);
        self.pending = None;
        self.idle_step()
    }

    /// Send the next scripted command, skipping LE ones when LE is absent.
    fn send_next(&mut self, now_ms: u64) -> Step {
        while let Some(it) = self.script.get(self.next) {
            if it.le && it.need != Need::LeProbe && !self.info.le_supported {
                self.next += 1;
                continue;
            }
            let cmd = it.cmd.clone();
            let timeout = if cmd == Command::Reset {
                RESET_TIMEOUT_MS
            } else {
                COMMAND_TIMEOUT_MS
            };
            if self.phase == Phase::FastPath && !it.fast {
                self.phase = Phase::SlowPath;
            }
            let deadline = now_ms.saturating_add(timeout);
            self.pending = Some((cmd.opcode(), deadline));
            return Step {
                send: Some(cmd),
                phase: self.phase,
                deadline_ms: Some(deadline),
            };
        }
        self.pending = None;
        self.phase = Phase::Complete;
        self.idle_step()
    }

    fn on_tick(&mut self, now_ms: u64) -> Step {
        let Some((opcode, deadline)) = self.pending else {
            return self.idle_step();
        };
        if now_ms < deadline {
            return self.idle_step();
        }
        if opcode == op::RESET && !self.reset_retried {
            self.reset_retried = true;
            return self.send_next(now_ms);
        }
        self.fail(InitError::Timeout { opcode })
    }

    fn on_event(&mut self, now_ms: u64, ev: &Event) -> Step {
        let Some((pending, _)) = self.pending else {
            return self.idle_step();
        };
        let (opcode, st, rp): (u16, u8, &[u8]) = match ev {
            Event::CommandComplete {
                opcode,
                return_params,
                ..
            } => (
                *opcode,
                ret::status(return_params).unwrap_or(0xFF),
                return_params,
            ),
            Event::CommandStatus { opcode, status, .. } => (*opcode, *status, &[]),
            _ => return self.idle_step(),
        };
        if opcode != pending {
            return self.idle_step();
        }
        self.finish_command(now_ms, opcode, st, rp)
    }

    fn finish_command(&mut self, now_ms: u64, opcode: u16, st: u8, rp: &[u8]) -> Step {
        let need = self.script[self.next].need;
        if st != status::SUCCESS {
            match need {
                Need::Required => {
                    return self.fail(InitError::CommandFailed { opcode, status: st })
                }
                Need::LeProbe => self.info.le_supported = false,
                Need::Optional => {}
            }
        } else if let Err(e) = self.record(opcode, rp) {
            return self.fail(e);
        }
        self.next += 1;
        self.send_next(now_ms)
    }

    /// Keep what a successful response tells us.
    fn record(&mut self, opcode: u16, rp: &[u8]) -> Result<(), InitError> {
        let bad = InitError::BadResponse { opcode };
        match opcode {
            op::READ_BUFFER_SIZE => self.info.acl = ret::parse_buffer_size(rp).map_err(|_| bad)?,
            op::LE_READ_BUFFER_SIZE => {
                self.info.le_acl = ret::parse_le_buffer_size(rp).map_err(|_| bad)?;
                self.info.le_supported = true;
            }
            op::READ_BD_ADDR => self.info.bd_addr = Some(ret::parse_bd_addr(rp).map_err(|_| bad)?),
            op::READ_LOCAL_VERSION => {
                self.info.version = Some(ret::parse_local_version(rp).map_err(|_| bad)?)
            }
            _ => {}
        }
        Ok(())
    }
}

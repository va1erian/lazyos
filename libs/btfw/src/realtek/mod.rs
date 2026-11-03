//! The Realtek firmware loader (RTL8761B/BU, RTL8822C/CE/CU, RTL8852 and
//! relatives), a step function over the documented patch download.
//!
//! ```text
//! Read_Local_Version (0x1001)   LMP subversion, HCI revision -> which chip?
//!   unknown chip: Realtek "drop firmware" (0xFC66), wait 200 ms, ask again;
//!   still unknown: nothing to load
//! Read_ROM_Version (0xFC6D)     which patch inside the file
//! Read_Reg16 (0xFC61)           the security project register: signing key id
//! NeedFile rtl_bt/<chip>_fw.bin, then _config.bin (appended to the patch)
//! Download (0xFC20) x n         252 octets each, index in the first octet,
//!                               high bit on the last, each acknowledged
//! Read_Local_Version            the patched firmware's version
//! ```
//!
//! The commands, the 252-octet fragments and the index wrap (0x7f is followed
//! by 1) are the documented behaviour of Linux's `btrtl_initialize` and
//! `rtl_download_firmware` (`drivers/bluetooth/btrtl.c`).
//!
//! Left out: the UART variants, the RTL8822E detour through register reads,
//! and the extra `_v2` file lookup being limited to the RTL8852C (as in Linux).
//! The coredump hooks and the Microsoft vendor-extension opcode are not the
//! loader's business.

pub(crate) mod chips;
pub(crate) mod epatch;

use alloc::format;
use alloc::vec::Vec;

use bthci::opcode::READ_LOCAL_VERSION;
use bthci::ret::{parse_local_version, LocalVersion};

use chips::Chip;

use crate::link::{self, Link, Reply};
use crate::step::{failed, Config, Done, FileRequest, Input, Reason, Step};

const OP_DOWNLOAD: u16 = 0xfc20;
const OP_READ_REG16: u16 = 0xfc61;
const OP_DROP_FIRMWARE: u16 = 0xfc66;
const OP_ROM_VERSION: u16 = 0xfc6d;
/// Register read that returns the security project (the key id in byte 0).
const REG_SEC_PROJ: [u8; 5] = [0x10, 0xa4, 0xad, 0x00, 0xb0];
/// Octets of patch per Download command.
const FRAGMENT: usize = 252;
/// Linux waits this long after "drop firmware" before asking again.
const DROP_WAIT_MS: u64 = 200;
/// The index octet's last-fragment flag, and the last plain index (after
/// which the counter restarts at 1: index 0 means "first").
const LAST_FLAG: u8 = 0x80;
const WRAP_AT: u8 = 0x7f;

enum State {
    Idle,
    ReadVersion,
    /// "Drop firmware" sent; ask again when the wait is over.
    Dropping,
    ReadRom,
    ReadKey,
    /// The optional `_fw_v2.bin` of the RTL8852C.
    WantFirmwareV2,
    WantFirmware,
    WantConfig,
    Downloading,
    /// The download is complete; reading the new version proves it took.
    Verifying,
    Finished,
}

pub struct Realtek {
    state: State,
    link: Link,
    dropped: bool,
    chip: Option<&'static Chip>,
    hci_rev: u16,
    lmp_subver: u16,
    rom_version: u8,
    key_id: u8,
    firmware: Vec<u8>,
    payload: Vec<u8>,
    sent: usize,
    fragment: usize,
    fragments: usize,
    next_index: u8,
}

impl Default for Realtek {
    fn default() -> Self {
        Self::new()
    }
}

impl Realtek {
    pub fn new() -> Realtek {
        Realtek {
            state: State::Idle,
            link: Link::default(),
            dropped: false,
            chip: None,
            hci_rev: 0,
            lmp_subver: 0,
            rom_version: 0,
            key_id: 0,
            firmware: Vec::new(),
            payload: Vec::new(),
            sent: 0,
            fragment: 0,
            fragments: 0,
            next_index: 0,
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.link.deadline
    }

    pub fn step(&mut self, now: u64, input: Input<'_>) -> Step {
        match input {
            Input::Start if matches!(self.state, State::Idle) => self.read_version(now),
            Input::Event(packet) => self.on_event(now, packet),
            Input::File(data) => self.on_file(now, Some(data)),
            Input::NoFile => self.on_file(now, None),
            Input::Tick if self.link.expired(now) => self.on_timeout(now),
            Input::Detached => self.on_detached(),
            _ => Step::Wait,
        }
    }

    fn fail(&mut self, reason: Reason) -> Step {
        self.state = State::Finished;
        self.link.idle();
        failed(reason)
    }

    fn read_version(&mut self, now: u64) -> Step {
        self.state = State::ReadVersion;
        self.link.send(now, READ_LOCAL_VERSION, &[])
    }

    fn on_event(&mut self, now: u64, packet: &[u8]) -> Step {
        let opcode = self.link.opcode.unwrap_or(0);
        match self.link.reply(packet) {
            Reply::Complete(rp) => self.on_complete(now, opcode, rp),
            Reply::Refused(status) => self.fail(Reason::CommandFailed { opcode, status }),
            Reply::Vendor(_) | Reply::Other => Step::Wait,
        }
    }

    fn on_complete(&mut self, now: u64, opcode: u16, rp: Vec<u8>) -> Step {
        let bad = Reason::BadResponse { opcode };
        match self.state {
            State::ReadVersion => match parse_local_version(&rp) {
                Ok(version) => self.on_local_version(now, version),
                Err(_) => self.fail(bad),
            },
            State::ReadRom => match rp[..] {
                [_, rom] => {
                    self.rom_version = rom;
                    self.read_key(now)
                }
                _ => self.fail(bad),
            },
            State::ReadKey => match rp[..] {
                [_, key, _] => {
                    self.key_id = key;
                    self.want_firmware()
                }
                _ => self.fail(bad),
            },
            State::Downloading if rp.len() == 2 => self.send_fragment(now),
            State::Downloading => self.fail(bad),
            State::Verifying => match parse_local_version(&rp) {
                Ok(_) => {
                    self.state = State::Finished;
                    self.link.idle();
                    Step::Done(Done::Operational {
                        downloaded: true,
                        config: Config::NotUsed,
                    })
                }
                Err(_) => self.fail(bad),
            },
            _ => Step::Wait,
        }
    }

    fn on_local_version(&mut self, now: u64, version: LocalVersion) -> Step {
        let found = chips::find(
            version.lmp_subversion,
            version.hci_revision,
            version.hci_version,
        );
        let Some(chip) = found else {
            if self.dropped {
                // Still nothing we know: it is not a part we patch.
                self.state = State::Finished;
                self.link.idle();
                return Step::Done(Done::NoFirmwareNeeded);
            }
            // A controller running its vendor's stock image answers in a way
            // that matches nothing; this command makes it fall back to the
            // ROM, which then reports its real identity.
            self.dropped = true;
            self.state = State::Dropping;
            self.link.opcode = None;
            self.link.deadline = Some(now.saturating_add(DROP_WAIT_MS));
            return Step::Send(link::command(OP_DROP_FIRMWARE, &[]));
        };
        self.chip = Some(chip);
        self.hci_rev = version.hci_revision;
        self.lmp_subver = version.lmp_subversion;
        if chip.has_rom_version {
            self.state = State::ReadRom;
            self.link.send(now, OP_ROM_VERSION, &[])
        } else {
            self.read_key(now)
        }
    }

    fn read_key(&mut self, now: u64) -> Step {
        self.state = State::ReadKey;
        self.link.send(now, OP_READ_REG16, &REG_SEC_PROJ)
    }

    /// The firmware file name: the RTL8852C prefers a `_v2` image.
    fn want_firmware(&mut self) -> Step {
        let Some(chip) = self.chip else {
            return self.fail(Reason::BadResponse { opcode: 0 });
        };
        self.link.idle();
        let v2 = chip.lmp_subver == chips::LMP_8852A && self.hci_rev == 0x0c;
        let (state, suffix, required) = if v2 {
            (State::WantFirmwareV2, "_fw_v2.bin", false)
        } else {
            (State::WantFirmware, "_fw.bin", true)
        };
        self.state = state;
        Step::NeedFile(FileRequest {
            name: format!("rtl_bt/{}{suffix}", chip.stem),
            required,
        })
    }

    fn on_file(&mut self, now: u64, file: Option<Vec<u8>>) -> Step {
        match (&self.state, file) {
            (State::WantFirmwareV2, None) => {
                self.state = State::WantFirmware;
                let stem = self.chip.map_or("", |c| c.stem);
                Step::NeedFile(FileRequest {
                    name: format!("rtl_bt/{stem}_fw.bin"),
                    required: true,
                })
            }
            (State::WantFirmware, None) => self.fail(Reason::FileMissing),
            (State::WantFirmware | State::WantFirmwareV2, Some(data)) => {
                self.firmware = data;
                self.want_config(now)
            }
            (State::WantConfig, None) if self.chip.is_some_and(|c| c.config_needed) => {
                self.fail(Reason::FileMissing)
            }
            (State::WantConfig, None) => self.begin_download(now, Vec::new()),
            (State::WantConfig, Some(config)) => self.begin_download(now, config),
            _ => Step::Wait,
        }
    }

    /// Chips with a board configuration file get it appended to the patch,
    /// unless the controller is key-protected (Linux skips it then).
    fn want_config(&mut self, now: u64) -> Step {
        match self.chip {
            Some(chip) if chip.has_config && self.key_id == 0 => {
                self.state = State::WantConfig;
                Step::NeedFile(FileRequest {
                    name: format!("rtl_bt/{}_config.bin", chip.stem),
                    required: chip.config_needed,
                })
            }
            _ => self.begin_download(now, Vec::new()),
        }
    }

    fn begin_download(&mut self, now: u64, config: Vec<u8>) -> Step {
        let firmware = core::mem::take(&mut self.firmware);
        let mut payload = if self.lmp_subver == chips::LMP_8723A {
            // The oldest ROM takes a plain image, never an epatch.
            if epatch::is_epatch(&firmware) {
                return self.fail(Reason::BadFirmware("unexpected epatch signature"));
            }
            firmware
        } else {
            match epatch::build(&firmware, self.lmp_subver, self.rom_version, self.key_id) {
                Ok(patch) => patch,
                Err(why) => return self.fail(Reason::BadFirmware(why)),
            }
        };
        payload.extend_from_slice(&config);
        self.fragments = payload.len() / FRAGMENT + 1;
        self.payload = payload;
        self.sent = 0;
        self.fragment = 0;
        self.next_index = 0;
        self.send_fragment(now)
    }

    /// One Download command, or the final version read when none is left.
    /// The last fragment carries the remainder, which is empty when the
    /// patch is a whole number of fragments (that is what the controller
    /// expects: the flag travels in a fragment of its own).
    fn send_fragment(&mut self, now: u64) -> Step {
        if self.fragment >= self.fragments {
            self.state = State::Verifying;
            return self.link.send(now, READ_LOCAL_VERSION, &[]);
        }
        let last = self.fragment + 1 == self.fragments;
        let take = if last {
            self.payload.len() % FRAGMENT
        } else {
            FRAGMENT
        };
        let mut index = self.next_index;
        self.next_index = if index == WRAP_AT { 1 } else { index + 1 };
        if last {
            index |= LAST_FLAG;
        }
        let end = self.sent.saturating_add(take).min(self.payload.len());
        let mut params = Vec::with_capacity(1 + take);
        params.push(index);
        params.extend_from_slice(self.payload.get(self.sent..end).unwrap_or_default());
        self.sent = end;
        self.fragment += 1;
        self.state = State::Downloading;
        self.link.send(now, OP_DOWNLOAD, &params)
    }

    fn on_timeout(&mut self, now: u64) -> Step {
        if matches!(self.state, State::Dropping) {
            return self.read_version(now);
        }
        let opcode = self.link.opcode.unwrap_or(0);
        match self.state {
            State::Idle | State::Finished => Step::Wait,
            _ => self.fail(Reason::Timeout { opcode }),
        }
    }

    fn on_detached(&mut self) -> Step {
        match self.state {
            State::Idle | State::Finished => Step::Wait,
            // Every fragment was accepted, then the device left: it restarted
            // itself on the new image.
            State::Verifying => {
                self.state = State::Finished;
                self.link.idle();
                Step::Done(Done::Reenumerate)
            }
            _ => self.fail(Reason::Detached),
        }
    }
}

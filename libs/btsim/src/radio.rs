//! Vendor radios that need a firmware download before they are useful
//! (`docs/bluetooth-plan.md` B7): an Intel part that comes up in its
//! bootloader and a Realtek part with a ROM patch to take.
//!
//! The models speak the documented protocols closely enough for `libs/btfw`
//! to run end to end against them, and check what they are sent: fragments in
//! order, the Realtek index sequence and its last-fragment flag. They do not
//! verify signatures or run the image; a download "verifies" once the stream
//! has gone quiet, which is what a real bootloader's verdict looks like from
//! the host. Faults are injected by [`RadioFaults`], and a radio that drops
//! off the USB bus (Intel's recovery reset, a reboot into the new image) says
//! so through [`SimController::take_detach`] for the transport to act on.

use bthci::opcode::status;
use bthci::{ret, Command, Event};

use crate::ctrl::{SimController, Task};

/// Which controller is simulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// An AX201: legacy Read_Version record, `ibt-19-0-4.sfi`.
    IntelAx201,
    /// An AX211: TLV Read_Version, `ibt-0040-0041.sfi`.
    IntelAx211,
    /// An RTL8822C: `rtl8822cu_fw.bin`, no board configuration.
    Rtl8822cu,
    /// An RTL8761B dongle: `rtl8761bu_fw.bin` and its configuration.
    Rtl8761bu,
}

impl Part {
    /// The USB vendor and product id the part presents.
    pub fn usb_ids(self) -> (u16, u16) {
        match self {
            Part::IntelAx201 => (0x8087, 0x0026),
            Part::IntelAx211 => (0x8087, 0x0033),
            Part::Rtl8822cu => (0x0bda, 0xc822),
            Part::Rtl8761bu => (0x0bda, 0x8771),
        }
    }

    fn is_intel(self) -> bool {
        matches!(self, Part::IntelAx201 | Part::IntelAx211)
    }
}

/// What goes wrong, once each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioFaults {
    /// Intel: the bootloader's verdict on this many images, one after the
    /// other, is a rejection: the host sends the recovery reset and the part
    /// re-enumerates in its bootloader.
    pub reject_downloads: u32,
    /// Intel: the part drops off the USB bus with the reset that boots the
    /// image, and comes back running it.
    pub reenumerate_after_load: bool,
    /// Realtek: refuse this download fragment (0-based) with an HCI error.
    pub refuse_fragment: Option<usize>,
}

/// What the radio has seen, for tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioStats {
    /// Image fragments accepted (all attempts).
    pub fragments: usize,
    /// Octets of image command stream accepted (Intel type 1 fragments).
    pub stream_bytes: usize,
    /// The running image is the operational one.
    pub operational: bool,
    /// Times the part dropped off the bus.
    pub detaches: u32,
    /// DDC records written.
    pub ddc_records: usize,
}

/// How long the stream must be quiet before the bootloader gives its verdict.
const VERDICT_QUIET_MS: u64 = 120;
/// How long the part stays off the bus when it re-enumerates.
const REENUMERATE_MS: u64 = 400;
const OP_INTEL_RESET: u16 = 0xfc01;
const OP_READ_VERSION: u16 = 0xfc05;
const OP_SECURE_SEND: u16 = 0xfc09;
const OP_BOOT_PARAMS: u16 = 0xfc0d;
const OP_WRITE_DDC: u16 = 0xfc8b;
const OP_RTL_DOWNLOAD: u16 = 0xfc20;
const OP_RTL_REG16: u16 = 0xfc61;
const OP_RTL_ROM_VERSION: u16 = 0xfc6d;
const OP_RTL_DROP: u16 = 0xfc66;
const COMPANY_INTEL: u16 = 2;
const COMPANY_REALTEK: u16 = 93;

pub(crate) struct Radio {
    part: Part,
    faults: RadioFaults,
    stats: RadioStats,
    /// Bumped by every type 1 fragment: only the newest schedules a verdict.
    stream_generation: u32,
    /// The bootloader accepted the image (the verdict was sent).
    verdict_sent: bool,
    rejections: u32,
    /// Realtek: the next index the patch must carry (0 first, then 1..=0x7f).
    next_index: u8,
    /// Set when the part is about to drop off the bus (see `take_detach`).
    detach_pending: bool,
}

impl Radio {
    fn new(part: Part, faults: RadioFaults) -> Radio {
        Radio {
            part,
            faults,
            stats: RadioStats::default(),
            stream_generation: 0,
            verdict_sent: false,
            rejections: 0,
            next_index: 0,
            detach_pending: false,
        }
    }
}

fn put_tlv(out: &mut Vec<u8>, ty: u8, value: &[u8]) {
    out.push(ty);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
}

impl SimController {
    /// Make this controller one of the vendor radios, in the state it has
    /// when power comes on (Intel: bootloader; Realtek: ROM).
    pub fn set_radio(&mut self, part: Part, faults: RadioFaults) {
        self.radio = Some(Radio::new(part, faults));
    }

    pub fn radio_stats(&self) -> Option<RadioStats> {
        self.radio.as_ref().map(|r| r.stats)
    }

    /// The USB ids of the simulated part, if it is one.
    pub fn radio_usb_ids(&self) -> Option<(u16, u16)> {
        self.radio.as_ref().map(|r| r.part.usb_ids())
    }

    /// The radio dropped off the USB bus. Returns how long it stays off; the
    /// controller itself is back in its power-on state (Intel keeps the image
    /// it was running: the operational one after a boot, the bootloader after
    /// a recovery reset). The transport disconnects, waits, and reconnects.
    pub fn take_detach(&mut self, now_ms: u64) -> Option<u64> {
        self.run_until(now_ms);
        let radio = self.radio.as_mut().filter(|r| r.detach_pending)?;
        radio.detach_pending = false;
        radio.stats.detaches += 1;
        radio.verdict_sent = false;
        radio.next_index = 0;
        self.reset();
        self.discard_pending_output();
        self.credits = 1;
        Some(REENUMERATE_MS)
    }

    /// `Read_Local_Version` as this radio answers it.
    pub(crate) fn radio_version(&self) -> Option<ret::LocalVersion> {
        let radio = self.radio.as_ref()?;
        let (rev, sub) = match radio.part {
            Part::Rtl8822cu => (0xc, 0x8822),
            Part::Rtl8761bu => (0xb, 0x8761),
            Part::IntelAx201 | Part::IntelAx211 => (0x0f, 0x0001),
        };
        Some(ret::LocalVersion {
            hci_version: 0xa,
            hci_revision: rev,
            lmp_version: 0xa,
            manufacturer: if radio.part.is_intel() {
                COMPANY_INTEL
            } else {
                COMPANY_REALTEK
            },
            lmp_subversion: sub,
        })
    }

    /// Handle a vendor command. Returns false when the command is not the
    /// radio's (the generic controller answers it).
    pub(crate) fn radio_command(&mut self, cmd: &Command) -> bool {
        let Command::Unknown { opcode, params } = cmd else {
            return false;
        };
        let Some(part) = self.radio.as_ref().map(|r| r.part) else {
            return false;
        };
        if opcode >> 10 != 0x3f {
            return false;
        }
        let handled = if part.is_intel() {
            self.intel_command(*opcode, params)
        } else {
            self.realtek_command(*opcode, params)
        };
        if !handled {
            self.fast_complete(*opcode, vec![status::UNKNOWN_COMMAND]);
        }
        true
    }

    /// Command Complete without the generic latency: a download is thousands
    /// of commands and must not wait a millisecond for each.
    fn fast_complete(&mut self, opcode: u16, return_params: Vec<u8>) {
        let ev = Event::CommandComplete {
            num_commands: 1,
            opcode,
            return_params,
        };
        self.after(0, Task::Emit(ev));
    }

    fn vendor_event(&mut self, delay: u64, params: Vec<u8>) {
        self.after(delay, Task::Emit(Event::Unknown { code: 0xFF, params }));
    }

    fn radio_mut(&mut self) -> &mut Radio {
        self.radio.as_mut().expect("a radio command needs a radio")
    }

    // ---- Intel ------------------------------------------------------------

    fn intel_command(&mut self, opcode: u16, params: &[u8]) -> bool {
        match (opcode, params) {
            (OP_READ_VERSION, [0xff]) => {
                let answer = self.intel_tlv();
                self.fast_complete(opcode, answer);
            }
            (OP_READ_VERSION, []) => {
                let answer = self.intel_legacy();
                self.fast_complete(opcode, answer);
            }
            (OP_BOOT_PARAMS, _) => self.fast_complete(opcode, vec![0; 23]),
            (OP_SECURE_SEND, [ty, data @ ..]) => self.intel_fragment(*ty, data.len()),
            (OP_INTEL_RESET, [0, 1, 0, 1, ..]) => self.intel_boot(),
            (OP_INTEL_RESET, _) => {
                // The recovery reset: back to the bootloader, off the bus.
                let radio = self.radio_mut();
                radio.stats.operational = false;
                radio.detach_pending = true;
                self.fast_complete(opcode, vec![0]);
            }
            (OP_WRITE_DDC, _) => {
                self.radio_mut().stats.ddc_records += 1;
                self.fast_complete(opcode, vec![0]);
            }
            _ => return false,
        }
        true
    }

    fn intel_operational(&self) -> bool {
        self.radio.as_ref().is_some_and(|r| r.stats.operational)
    }

    fn intel_legacy(&self) -> Vec<u8> {
        let fw_variant = if self.intel_operational() { 0x23 } else { 0x06 };
        vec![0, 0x37, 0x13, 0, fw_variant, 4, 0, 0, 0, 0]
    }

    /// The TLV answer: the AX211's own, and the AX201's record of the middle
    /// generation (its bootloader names the file by the legacy record).
    fn intel_tlv(&self) -> Vec<u8> {
        let part = self.radio.as_ref().map(|r| r.part);
        let (hw_variant, tops) = match part {
            Some(Part::IntelAx201) => (0x13u8, (0x410u32, 0x410u32)),
            _ => (0x1b, (0x400, 0x410)),
        };
        let image = if self.intel_operational() { 3 } else { 1 };
        let mut out = vec![0u8];
        put_tlv(&mut out, 0x10, &tops.0.to_le_bytes());
        put_tlv(&mut out, 0x11, &tops.1.to_le_bytes());
        let cnvi_bt = 0x37u32 << 8 | u32::from(hw_variant) << 16;
        put_tlv(&mut out, 0x12, &cnvi_bt.to_le_bytes());
        put_tlv(&mut out, 0x1c, &[image]);
        // The ECDSA engine: the image carries both signature blocks.
        put_tlv(&mut out, 0x2f, &[1]);
        put_tlv(&mut out, 0x2d, &[0, 0, 0]);
        out
    }

    fn intel_fragment(&mut self, ty: u8, len: usize) {
        let radio = self.radio_mut();
        radio.stats.fragments += 1;
        radio.stream_generation = radio.stream_generation.wrapping_add(1);
        if ty == 1 {
            radio.stats.stream_bytes += len;
            let generation = radio.stream_generation;
            self.after(VERDICT_QUIET_MS, Task::RadioVerdict { generation });
        }
        self.fast_complete(OP_SECURE_SEND, vec![0]);
    }

    /// The stream has been quiet: the bootloader checks the image and says so.
    pub(crate) fn radio_verdict(&mut self, generation: u32) {
        let radio = self.radio_mut();
        if radio.stream_generation != generation || radio.verdict_sent {
            return;
        }
        radio.verdict_sent = true;
        let rejected = radio.rejections < radio.faults.reject_downloads;
        radio.rejections += u32::from(rejected);
        let result = u8::from(rejected) * 0x12;
        self.vendor_event(0, vec![0x06, result, 0x09, 0xfc, 0]);
    }

    /// The reset that boots the image the bootloader accepted.
    fn intel_boot(&mut self) {
        let radio = self.radio_mut();
        radio.stats.operational = true;
        let leaves = radio.faults.reenumerate_after_load && radio.stats.detaches == 0;
        self.fast_complete(OP_INTEL_RESET, vec![0]);
        self.vendor_event(10, vec![0x02, 0, 5, 1, 0, 0, 1]);
        if leaves {
            self.radio_mut().detach_pending = true;
        }
    }

    // ---- Realtek ----------------------------------------------------------

    fn realtek_command(&mut self, opcode: u16, params: &[u8]) -> bool {
        match (opcode, params) {
            (OP_RTL_ROM_VERSION, _) => {
                let rom = if self.radio_part() == Some(Part::Rtl8822cu) {
                    3
                } else {
                    1
                };
                self.fast_complete(opcode, vec![0, rom]);
            }
            (OP_RTL_REG16, _) => self.fast_complete(opcode, vec![0, 0, 0]),
            (OP_RTL_DOWNLOAD, [index, _data @ ..]) => self.realtek_fragment(*index),
            // A part that already runs a patch would fall back to its ROM;
            // this one always starts there, so there is nothing to drop.
            (OP_RTL_DROP, _) => {}
            _ => return false,
        }
        true
    }

    fn radio_part(&self) -> Option<Part> {
        self.radio.as_ref().map(|r| r.part)
    }

    fn realtek_fragment(&mut self, index: u8) {
        let radio = self.radio_mut();
        let n = radio.stats.fragments;
        if radio.faults.refuse_fragment == Some(n) {
            return self.fast_complete(OP_RTL_DOWNLOAD, vec![0x03]);
        }
        let plain = index & 0x7f;
        if plain != radio.next_index {
            return self.fast_complete(OP_RTL_DOWNLOAD, vec![status::INVALID_PARAMETERS]);
        }
        radio.next_index = if plain == 0x7f { 1 } else { plain + 1 };
        radio.stats.fragments += 1;
        if index & 0x80 != 0 {
            radio.stats.operational = true;
        }
        self.fast_complete(OP_RTL_DOWNLOAD, vec![0, index]);
    }
}

//! The Intel firmware loader (iBT controllers: AX200, AX201, AX210, AX211,
//! 8260, 9260, ...), a step function over the documented download protocol.
//!
//! ```text
//! Read_Version (0xFC05)        legacy record, or TLV list -> bootloader / operational?
//!   operational: done, nothing to load
//! [Read_Boot_Params (0xFC0D)]  legacy parts only: device revision, fragment acks
//! NeedFile intel/ibt-*.sfi     the name comes from the version answer
//! Secure_Send (0xFC09) x n     CSS header, public key, signature, then the
//!                              command stream in 4-octet-aligned groups,
//!                              252 octets per fragment, each acknowledged
//! vendor event 0x06            the bootloader's verdict on the whole image
//! Intel_Reset (0xFC01)         boot the operational image
//! vendor event 0x02            "boot up complete": same USB device (Linux's
//!                              wait); a detach here is the expected
//!                              re-enumeration and is reported as such
//! NeedFile intel/ibt-*.ddc     optional; records via Write_DDC (0xFC8B)
//! ```
//!
//! Anything that goes wrong while the bootloader holds a half-written image
//! sends the recovery reset (reset type 1, reload everything) so the next
//! attempt starts from the bootloader; the failure says the device will come
//! back as a new USB device.
//!
//! Left out: the "intermediate loader" and "unlocker" flows of the newest
//! (Blazar, Gale Peak 2 and later) parts, the ROM-patch (`.bseq`) flow of the
//! 7260/3160 generation, and the DSBR / SAR / PPAG settings: none of them is
//! needed to bring up a keyboard and mouse on the boxes the plan targets, and
//! each is reported as [`Reason::Unsupported`] so a caller can fall back.
//! Source for the protocol: Linux `drivers/bluetooth/btintel.c`.

pub(crate) mod ddc;
pub(crate) mod sfi;
pub(crate) mod version;

use alloc::vec::Vec;

use sfi::{Layout, Sfi, FRAG_DATA};
use version::{BootParams, Legacy, Tlv};

use crate::link::{Link, Reply};
use crate::step::{failed, Config, Done, Failure, FileRequest, Input, Reason, Step};

const OP_RESET: u16 = 0xfc01;
const OP_READ_VERSION: u16 = 0xfc05;
const OP_SECURE_SEND: u16 = 0xfc09;
const OP_BOOT_PARAMS: u16 = 0xfc0d;
const OP_WRITE_DDC: u16 = 0xfc8b;
/// The most data one Secure_Send command carries (255 minus the type octet,
/// rounded down to a multiple of 4 as the bootloader expects).
const FRAGMENT: usize = 252;
/// Vendor event subevents: the bootloader's download verdict and the
/// operational firmware's "boot up" notice.
const EV_BOOTUP: u8 = 0x02;
const EV_RESULT: u8 = 0x06;
/// `Intel_Reset` that boots the image just loaded: soft reset, patches on,
/// DDC not reloaded, boot from the given address.
const RESET_BOOT: [u8; 4] = [0x00, 0x01, 0x00, 0x01];
/// `Intel_Reset` that drops back to the bootloader and re-enumerates: hard
/// reset, patches on, DDC reload, current image, boot address 0.
const RESET_RECOVER: [u8; 8] = [0x01, 0x01, 0x01, 0x00, 0, 0, 0, 0];

/// What the version answer taught us, kept to name the later files.
enum Ident {
    Legacy(Legacy, BootParams),
    Tlv(Tlv),
}

enum State {
    Idle,
    ReadVersion,
    /// A TLV answer from a middle-generation part: ask again, legacy style.
    ReadLegacy,
    BootParams(Legacy),
    WantSfi,
    Downloading,
    AwaitResult,
    Resetting,
    WantDdc,
    Ddc,
    /// The recovery reset is in flight; the failure to report afterwards.
    Recovering(Reason),
    Finished,
}

/// Where the download stands: which header fragment, and the run of the file
/// being cut into 252-octet pieces.
#[derive(Default)]
struct Cursor {
    header: usize,
    /// `(fragment type, next offset, end)` of the piece in progress.
    run: Option<(u8, usize, usize)>,
    next_group: usize,
}

pub struct Intel {
    state: State,
    link: Link,
    ident: Option<Ident>,
    sfi: Option<Sfi>,
    cursor: Cursor,
    /// The bootloader's verdict, once it has sent it.
    verdict_ok: bool,
    boot_param: u32,
    downloaded: bool,
    ddc: Vec<u8>,
    ddc_pos: usize,
    ddc_done: u16,
}

impl Default for Intel {
    fn default() -> Self {
        Self::new()
    }
}

impl Intel {
    pub fn new() -> Intel {
        Intel {
            state: State::Idle,
            link: Link::default(),
            ident: None,
            sfi: None,
            cursor: Cursor::default(),
            verdict_ok: false,
            boot_param: 0,
            downloaded: false,
            ddc: Vec::new(),
            ddc_pos: 0,
            ddc_done: 0,
        }
    }

    /// When to call [`Input::Tick`] if nothing else happens first.
    pub fn deadline(&self) -> Option<u64> {
        self.link.deadline
    }

    pub fn step(&mut self, now: u64, input: Input<'_>) -> Step {
        match input {
            Input::Start if matches!(self.state, State::Idle) => {
                self.state = State::ReadVersion;
                self.link.send(now, OP_READ_VERSION, &[0xff])
            }
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

    fn finish(&mut self, config: Config) -> Step {
        self.state = State::Finished;
        self.link.idle();
        Step::Done(Done::Operational {
            downloaded: self.downloaded,
            config,
        })
    }

    /// Give up on the half-loaded bootloader: reset it so the next attempt
    /// starts clean, then report `reason`.
    fn recover(&mut self, now: u64, reason: Reason) -> Step {
        if matches!(self.state, State::Recovering(_)) {
            return self.fail_reenumerating(reason);
        }
        self.state = State::Recovering(reason);
        self.link.send(now, OP_RESET, &RESET_RECOVER)
    }

    fn fail_reenumerating(&mut self, reason: Reason) -> Step {
        self.state = State::Finished;
        self.link.idle();
        Step::Failed(Failure {
            reason,
            reenumerate: true,
        })
    }

    fn on_event(&mut self, now: u64, packet: &[u8]) -> Step {
        let opcode = self.link.opcode.unwrap_or(0);
        match self.link.reply(packet) {
            Reply::Complete(rp) => self.on_complete(now, rp),
            Reply::Refused(status) => {
                self.on_refused(now, Reason::CommandFailed { opcode, status })
            }
            Reply::Vendor(params) => self.on_vendor(now, &params),
            Reply::Other => Step::Wait,
        }
    }

    fn on_complete(&mut self, now: u64, rp: Vec<u8>) -> Step {
        match core::mem::replace(&mut self.state, State::Finished) {
            State::ReadVersion => self.on_version(now, rp),
            State::ReadLegacy => match version::parse_legacy(&rp) {
                Some(legacy) => self.begin_legacy(now, legacy),
                None => self.fail(Reason::BadResponse {
                    opcode: OP_READ_VERSION,
                }),
            },
            State::BootParams(legacy) => self.on_boot_params(legacy, &rp),
            State::Downloading => self.send_fragment(now),
            State::Resetting => {
                self.state = State::Resetting;
                Step::Wait
            }
            State::Ddc => {
                self.ddc_done += 1;
                self.ddc_next(now)
            }
            State::Recovering(reason) => self.fail_reenumerating(reason),
            other => {
                self.state = other;
                Step::Wait
            }
        }
    }

    fn on_refused(&mut self, now: u64, reason: Reason) -> Step {
        match self.state {
            State::Downloading | State::Resetting => self.recover(now, reason),
            State::Ddc => self.finish(Config::Failed),
            State::Recovering(first) => self.fail_reenumerating(first),
            State::Idle | State::Finished => Step::Wait,
            _ => self.fail(reason),
        }
    }

    fn on_vendor(&mut self, now: u64, params: &[u8]) -> Step {
        match (&self.state, params) {
            (State::Downloading | State::AwaitResult, [EV_RESULT, result, _, _, _]) => {
                if *result != 0 {
                    let reason = Reason::DownloadRejected { result: *result };
                    return self.recover(now, reason);
                }
                self.verdict_ok = true;
                if matches!(self.state, State::AwaitResult) {
                    return self.start_boot(now);
                }
                Step::Wait
            }
            (State::Resetting, [EV_BOOTUP, _, _, _, _, _, _]) => self.boot_done(),
            _ => Step::Wait,
        }
    }

    fn on_version(&mut self, now: u64, rp: Vec<u8>) -> Step {
        if let Some(legacy) = version::parse_legacy(&rp) {
            return self.begin_legacy(now, legacy);
        }
        let tlv = match version::parse_tlv(rp.get(1..).unwrap_or_default()) {
            Ok(tlv) => tlv,
            Err(_) => {
                return self.fail(Reason::BadResponse {
                    opcode: OP_READ_VERSION,
                })
            }
        };
        if tlv.hw_platform() != version::HW_PLATFORM {
            return self.fail(Reason::Unsupported("not an Intel iBT platform"));
        }
        match tlv.hw_variant() {
            // Their operational firmware answers TLV too, but the bootloader
            // names its file by the legacy record: ask that way.
            0x11..=0x14 => {
                self.state = State::ReadLegacy;
                self.link.send(now, OP_READ_VERSION, &[])
            }
            0x17..=0x1b => self.begin_tlv(tlv),
            _ => self.fail(Reason::Unsupported("Intel hardware variant")),
        }
    }

    fn begin_legacy(&mut self, now: u64, legacy: Legacy) -> Step {
        match legacy.hw_variant {
            0x0b | 0x0c | 0x11..=0x14 => {}
            _ => return self.fail(Reason::Unsupported("Intel legacy hardware variant")),
        }
        match legacy.fw_variant {
            version::FW_OPERATIONAL => self.finish(Config::NotUsed),
            version::FW_BOOTLOADER => {
                self.state = State::BootParams(legacy);
                self.link.send(now, OP_BOOT_PARAMS, &[])
            }
            _ => self.fail(Reason::Unsupported("Intel firmware variant")),
        }
    }

    fn begin_tlv(&mut self, tlv: Tlv) -> Step {
        match tlv.img_type {
            version::IMG_OPERATIONAL => self.finish(Config::NotUsed),
            version::IMG_BOOTLOADER => {
                let name = version::tlv_name(&tlv, "sfi");
                self.ident = Some(Ident::Tlv(tlv));
                self.want_sfi(name)
            }
            version::IMG_IML => self.fail(Reason::Unsupported("Intel intermediate loader image")),
            _ => self.fail(Reason::Unsupported("Intel image type")),
        }
    }

    fn on_boot_params(&mut self, legacy: Legacy, rp: &[u8]) -> Step {
        let Some(params) = version::parse_boot_params(rp) else {
            return self.fail(Reason::BadResponse {
                opcode: OP_BOOT_PARAMS,
            });
        };
        if params.limited_cce != 0 {
            // The download below relies on an acknowledgement per fragment.
            return self.fail(Reason::Unsupported("bootloader without fragment acks"));
        }
        let Some(name) = version::legacy_name(&legacy, &params, "sfi") else {
            return self.fail(Reason::Unsupported("Intel firmware naming"));
        };
        self.ident = Some(Ident::Legacy(legacy, params));
        self.want_sfi(name)
    }

    fn want_sfi(&mut self, name: alloc::string::String) -> Step {
        self.state = State::WantSfi;
        self.link.idle();
        Step::NeedFile(FileRequest {
            name,
            required: true,
        })
    }

    fn on_file(&mut self, now: u64, file: Option<Vec<u8>>) -> Step {
        match (&self.state, file) {
            (State::WantSfi, None) => self.fail(Reason::FileMissing),
            (State::WantSfi, Some(data)) => self.begin_download(now, data),
            (State::WantDdc, None) => self.finish(Config::Missing),
            (State::WantDdc, Some(data)) => match ddc::validate(&data) {
                Ok(_) => {
                    self.ddc = data;
                    self.ddc_next(now)
                }
                Err(_) => self.finish(Config::Failed),
            },
            _ => Step::Wait,
        }
    }

    fn begin_download(&mut self, now: u64, data: Vec<u8>) -> Step {
        let (layout, expected) = match &self.ident {
            Some(Ident::Legacy(legacy, _)) => (Layout::Rsa, legacy.fw_build),
            Some(Ident::Tlv(tlv)) => (
                Layout::Both {
                    sbe_type: tlv.sbe_type,
                },
                tlv.min_fw,
            ),
            None => return self.fail(Reason::BadResponse { opcode: 0 }),
        };
        let sfi = match Sfi::parse(data, layout) {
            Ok(sfi) => sfi,
            Err(why) => return self.fail(Reason::BadFirmware(why)),
        };
        self.boot_param = sfi.boot_addr;
        if sfi.version == Some(expected) {
            // The controller already holds this build: just boot it.
            return self.start_boot(now);
        }
        self.cursor = Cursor {
            next_group: sfi.body_start(),
            ..Cursor::default()
        };
        self.sfi = Some(sfi);
        self.downloaded = true;
        self.send_fragment(now)
    }

    /// The next piece of the file as Secure_Send parameters: the type octet
    /// then at most [`FRAGMENT`] octets, headers first, then the command groups.
    fn next_fragment(&mut self) -> Option<Vec<u8>> {
        let sfi = self.sfi.as_ref()?;
        let cur = &mut self.cursor;
        let (ty, pos, end) = match cur.run.take() {
            Some(run) => run,
            None => {
                if let Some((ty, range)) = sfi.headers().get(cur.header) {
                    cur.header += 1;
                    (*ty, range.start, range.end)
                } else {
                    let start = cur.next_group;
                    let end = sfi.next_group(start)?;
                    cur.next_group = end;
                    (FRAG_DATA, start, end)
                }
            }
        };
        let stop = end.min(pos + FRAGMENT);
        if stop < end {
            cur.run = Some((ty, stop, end));
        }
        let mut params = Vec::with_capacity(1 + stop - pos);
        params.push(ty);
        params.extend_from_slice(sfi.bytes(pos..stop));
        Some(params)
    }

    fn send_fragment(&mut self, now: u64) -> Step {
        if let Some(params) = self.next_fragment() {
            self.state = State::Downloading;
            return self.link.send(now, OP_SECURE_SEND, &params);
        }
        if self.verdict_ok {
            return self.start_boot(now);
        }
        // Every fragment is acknowledged: now the bootloader says whether the
        // image as a whole verified.
        self.state = State::AwaitResult;
        self.link.wait_event(now);
        Step::Wait
    }

    fn start_boot(&mut self, now: u64) -> Step {
        self.state = State::Resetting;
        self.sfi = None;
        let mut params = RESET_BOOT.to_vec();
        params.extend_from_slice(&self.boot_param.to_le_bytes());
        self.link.send(now, OP_RESET, &params)
    }

    /// The operational firmware announced itself: ask for the device
    /// configuration file, if this part has a name for one.
    fn boot_done(&mut self) -> Step {
        let name = match &self.ident {
            Some(Ident::Legacy(legacy, params)) => version::legacy_name(legacy, params, "ddc"),
            Some(Ident::Tlv(tlv)) => Some(version::tlv_name(tlv, "ddc")),
            None => None,
        };
        let Some(name) = name else {
            return self.finish(Config::NotUsed);
        };
        self.state = State::WantDdc;
        self.link.idle();
        Step::NeedFile(FileRequest {
            name,
            required: false,
        })
    }

    fn ddc_next(&mut self, now: u64) -> Step {
        match ddc::record(&self.ddc, self.ddc_pos) {
            Some(range) => {
                self.ddc_pos = range.end;
                self.state = State::Ddc;
                let params = self.ddc.get(range).unwrap_or_default().to_vec();
                self.link.send(now, OP_WRITE_DDC, &params)
            }
            None => self.finish(Config::Applied(self.ddc_done)),
        }
    }

    fn on_timeout(&mut self, now: u64) -> Step {
        let reason = Reason::Timeout {
            opcode: self.link.opcode.unwrap_or(0),
        };
        match self.state {
            State::Downloading | State::AwaitResult | State::Resetting => self.recover(now, reason),
            State::Ddc => self.finish(Config::Failed),
            State::Recovering(first) => self.fail_reenumerating(first),
            State::Idle | State::Finished => Step::Wait,
            _ => self.fail(reason),
        }
    }

    fn on_detached(&mut self) -> Step {
        match self.state {
            // The reset that boots the new image may take the USB device with
            // it: that is the expected re-enumeration, not a failure.
            State::Resetting => {
                self.state = State::Finished;
                self.link.idle();
                Step::Done(Done::Reenumerate)
            }
            State::Recovering(reason) => self.fail_reenumerating(reason),
            State::Downloading | State::AwaitResult => self.fail_reenumerating(Reason::Detached),
            State::Idle | State::Finished => Step::Wait,
            _ => self.fail(Reason::Detached),
        }
    }
}

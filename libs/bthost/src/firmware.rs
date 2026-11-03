//! The firmware phase of bring-up (`docs/bluetooth-plan.md` B7).
//!
//! A real radio (Intel, Realtek) needs its vendor's firmware before it is
//! useful. When the shell tells [`Host::new_for`] the controller's USB ids,
//! the vendor they name runs `libs/btfw`'s [`Loader`] first; `bthci::Init`
//! (which starts with `HCI_Reset`) begins only once the loader is done. A
//! controller no loader applies to (`Vendor::Generic`: the simulator, a CSR
//! dongle) goes straight to `Init`, with no line of its own.
//!
//! The host stays sans-IO. The loader's commands leave through
//! [`Host::poll_command`] like any other; the files it names leave as
//! [`Effect::LoadFirmware`], which the shell answers (from `libs/fwload`)
//! with [`Host::on_firmware_file`]; its 200 ms and 5 s deadlines are part of
//! [`Host::next_deadline`], so nothing polls.
//!
//! **Re-enumeration.** A controller that resets itself drops off the USB bus
//! and comes back as a new device (Intel's recovery reset after a failed
//! download; its boot reset on some parts). `usbd` withdraws the transport and
//! publishes it again, and the shell starts a fresh host. The count of
//! re-enumerations so far travels from host to host
//! ([`Host::firmware_attempts`]) and is cleared once a bring-up completes, so
//! a radio that resets itself forever stops after [`MAX_REENUMERATIONS`] with
//! `FW:FAIL`. Intel's *successful* download does not re-enumerate.
//!
//! Evidence (`BT:` prefix added by the shell): `FW:VENDOR`, `FW:FILE name=`,
//! `FW:DONE downloaded= config= ms=`, `FW:REENUM attempt=`, `FW:FAIL reason=`.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use btfw::{Config, Done, Failure, Input, Loader, Reason, Step, Vendor};
use bthci::Input as InitInput;

use crate::effect::Effect;
use crate::host::Host;

/// Most re-enumerations one bring-up puts up with before it gives up.
pub const MAX_REENUMERATIONS: u32 = 3;
/// How long a controller that was told to reset has to leave the bus.
const REENUMERATE_WAIT_MS: u64 = 10_000;

/// The USB vendor and product ids of the controller, from `Describe()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbIds {
    pub vendor: u16,
    pub product: u16,
}

enum Stage {
    /// The loader is running.
    Loading(Box<Loader>),
    /// A reset was sent: nothing more happens until the device leaves.
    AwaitingReenumeration { until: u64 },
}

pub(crate) struct Firmware {
    stage: Stage,
    vendor: Vendor,
    started: u64,
    /// Commands the loader produced, waiting for the shell.
    out: VecDeque<Vec<u8>>,
    /// The file the last [`Step::NeedFile`] named.
    asked: Option<String>,
}

impl Firmware {
    pub(crate) fn deadline(&self) -> Option<u64> {
        match &self.stage {
            Stage::Loading(loader) => loader.deadline(),
            Stage::AwaitingReenumeration { until } => Some(*until),
        }
    }
}

fn vendor_name(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Intel => "intel",
        Vendor::Realtek => "realtek",
        Vendor::Generic => "generic",
    }
}

fn config_name(config: Config) -> String {
    match config {
        Config::NotUsed => "none".into(),
        Config::Missing => "missing".into(),
        Config::Applied(n) => format!("applied:{n}"),
        Config::Failed => "failed".into(),
    }
}

/// A reason as one lowercase token-ish phrase for the trace (no spaces
/// inside a value that a judge splits on; free text goes last).
fn reason_text(reason: Reason) -> String {
    match reason {
        Reason::CommandFailed { opcode, status } => {
            format!("command-failed opcode={opcode:#06x} status={status:#04x}")
        }
        Reason::Timeout { opcode } => format!("timeout opcode={opcode:#06x}"),
        Reason::BadResponse { opcode } => format!("bad-response opcode={opcode:#06x}"),
        Reason::Unsupported(what) => format!("unsupported {what}"),
        Reason::FileMissing => "file-missing".into(),
        Reason::BadFirmware(where_) => format!("bad-firmware {where_}"),
        Reason::DownloadRejected { result } => format!("download-rejected result={result:#04x}"),
        Reason::Detached => "detached".into(),
    }
}

impl Host {
    /// Start a host for a controller with these USB ids (`None`: unknown, so
    /// no firmware phase). `reenumerations` is the count the previous host
    /// handed over ([`Host::firmware_attempts`]; 0 for a first start).
    pub fn new_for(
        cfg: crate::Config,
        store: btbond::BondStore,
        now: u64,
        usb: Option<UsbIds>,
        reenumerations: u32,
    ) -> Host {
        let mut host = Host::build(cfg, store, now);
        host.fw_reenumerations = reenumerations;
        if reenumerations > MAX_REENUMERATIONS {
            // The last host already gave up and the controller left anyway:
            // touch nothing (a reset would only start the loop again). The
            // count stays above the cap, so every later host does the same;
            // only a successful bring-up (`CTRL:PASS`) clears it.
            host.note(format!(
                "FW:FAIL reason=reenumeration-cap attempts={reenumerations} ms=0"
            ));
            host.fail("the controller keeps resetting".into());
            return host;
        }
        let vendor = usb.map_or(Vendor::Generic, |ids| {
            Vendor::from_usb(ids.vendor, ids.product)
        });
        if vendor == Vendor::Generic {
            host.init_step(now, InitInput::Start);
            return host;
        }
        let ids = usb.unwrap_or(UsbIds {
            vendor: 0,
            product: 0,
        });
        host.note(format!(
            "FW:VENDOR vendor={} usb={:04x}:{:04x} attempt={}",
            vendor_name(vendor),
            ids.vendor,
            ids.product,
            reenumerations
        ));
        host.fw = Some(Firmware {
            stage: Stage::Loading(Box::new(Loader::new(vendor))),
            vendor,
            started: now,
            out: VecDeque::new(),
            asked: None,
        });
        host.fw_feed(now, Input::Start);
        host
    }

    /// Re-enumerations so far in this bring-up, to hand to the next host.
    /// Zero once the controller has come up.
    pub fn firmware_attempts(&self) -> u32 {
        self.fw_reenumerations
    }

    /// Whether the firmware phase is still running.
    pub fn loading_firmware(&self) -> bool {
        self.fw.is_some()
    }

    /// The shell's answer to [`Effect::LoadFirmware`]: the file, or `None`
    /// when it does not exist or could not be read.
    pub fn on_firmware_file(&mut self, now: u64, file: Option<Vec<u8>>) {
        let Some(fw) = self.fw.as_mut() else {
            return;
        };
        let Some(name) = fw.asked.take() else {
            // Nobody asked: a confused shell must not feed the loader.
            return;
        };
        match &file {
            Some(bytes) => self.note(format!("FW:FILE name={name} bytes={}", bytes.len())),
            None => self.note(format!("FW:FILE name={name} missing=1")),
        }
        self.fw_feed(
            now,
            match file {
                Some(bytes) => Input::File(bytes),
                None => Input::NoFile,
            },
        );
    }

    /// The USB device went away while the host ran (the shell saw the
    /// transport's `Gone`).
    pub fn on_detached(&mut self, now: u64) {
        if matches!(
            self.fw.as_ref().map(|fw| &fw.stage),
            Some(Stage::Loading(_))
        ) {
            self.fw_feed(now, Input::Detached);
        }
    }

    // ---- hooks the entry points call while the phase runs ------------------

    /// An HCI event from the controller, while the phase runs.
    pub(crate) fn fw_event(&mut self, now: u64, packet: &[u8]) {
        self.fw_feed(now, Input::Event(packet));
    }

    /// The deadline passed.
    pub(crate) fn fw_tick(&mut self, now: u64) {
        let Some(fw) = self.fw.as_ref() else {
            return;
        };
        if fw.deadline().is_none_or(|due| due > now) {
            return;
        }
        match fw.stage {
            Stage::Loading(_) => self.fw_feed(now, Input::Tick),
            Stage::AwaitingReenumeration { .. } => {
                self.fw = None;
                self.note("FW:FAIL reason=no-reenumeration".into());
                self.fail("the controller did not re-enumerate after its reset".into());
            }
        }
    }

    pub(crate) fn fw_poll_command(&mut self) -> Option<Vec<u8>> {
        self.fw.as_mut()?.out.pop_front()
    }

    // ---- the loader's steps ------------------------------------------------

    fn fw_feed(&mut self, now: u64, input: Input<'_>) {
        let step = match self.fw.as_mut().map(|fw| &mut fw.stage) {
            Some(Stage::Loading(loader)) => loader.step(now, input),
            _ => return,
        };
        match step {
            Step::Send(packet) => {
                if let Some(fw) = self.fw.as_mut() {
                    fw.out.push_back(packet);
                }
            }
            Step::NeedFile(request) => {
                let line = format!(
                    "FW:FILE name={} required={}",
                    request.name, request.required
                );
                if let Some(fw) = self.fw.as_mut() {
                    fw.asked = Some(request.name.clone());
                }
                self.note(line);
                self.effect(Effect::LoadFirmware {
                    name: request.name,
                    required: request.required,
                });
            }
            Step::Wait => {}
            Step::Done(done) => self.fw_done(now, done),
            Step::Failed(failure) => self.fw_failed(now, failure),
        }
    }

    fn fw_elapsed(&self, now: u64) -> u64 {
        self.fw
            .as_ref()
            .map_or(0, |fw| now.saturating_sub(fw.started))
    }

    fn fw_done(&mut self, now: u64, done: Done) {
        let ms = self.fw_elapsed(now);
        let vendor = self.fw.as_ref().map_or(Vendor::Generic, |fw| fw.vendor);
        let (downloaded, config) = match done {
            Done::NoFirmwareNeeded => (false, Config::NotUsed),
            Done::Operational { downloaded, config } => (downloaded, config),
            Done::Reenumerate => return self.fw_reenumerate(now, None),
        };
        self.note(format!(
            "FW:DONE vendor={} downloaded={} config={} ms={ms}",
            vendor_name(vendor),
            u8::from(downloaded),
            config_name(config)
        ));
        self.fw = None;
        self.init_step(now, InitInput::Start);
    }

    fn fw_failed(&mut self, now: u64, failure: Failure) {
        if failure.reenumerate {
            return self.fw_reenumerate(now, Some(failure.reason));
        }
        let ms = self.fw_elapsed(now);
        self.note(format!(
            "FW:FAIL reason={} ms={ms}",
            reason_text(failure.reason)
        ));
        self.fw = None;
        self.fail("firmware load failed".into());
    }

    /// The controller reset itself (or is about to): count it, give up past
    /// the cap, otherwise wait for the shell to restart the bring-up when the
    /// device returns.
    fn fw_reenumerate(&mut self, now: u64, reason: Option<Reason>) {
        self.fw_reenumerations += 1;
        let attempt = self.fw_reenumerations;
        let ms = self.fw_elapsed(now);
        if attempt > MAX_REENUMERATIONS {
            self.fw = None;
            self.note(format!(
                "FW:FAIL reason=reenumeration-cap attempts={attempt} ms={ms}"
            ));
            return self.fail("the controller keeps resetting".into());
        }
        let because = reason.map_or(String::from("reset"), reason_text);
        self.note(format!(
            "FW:REENUM attempt={attempt} ms={ms} reason={because}"
        ));
        if let Some(fw) = self.fw.as_mut() {
            fw.out.clear();
            fw.stage = Stage::AwaitingReenumeration {
                until: now.saturating_add(REENUMERATE_WAIT_MS),
            };
        }
    }
}

//! The radio: `libs/rtw88`'s bring-up on the real card
//! (docs/wifi-rtw88-plan.md R3), and the diagnostic receive survey
//! (`mode=survey`) that runs instead of the station service when asked.
//!
//! [`bring_up`] follows upstream's probe and start (`rtw_pci_probe`,
//! `rtw_core_start`): the PCIe rings over `bus.regs` and `bus.dma`, the chip
//! identity and efuse (`WIFID:CHIP:INFO`), the PCIe PHY parameters, power-on
//! with the firmware download, MAC/BB/RF init and calibration
//! (`WIFID:FW:READY`), the station port (`WIFID:MAC`), and the antenna
//! grants read back (`WIFID:COEX`). Any `rtw88::Error` is a
//! [`Failure`] naming the step; `wifid.rs` prints it with the register dump
//! and parks with the claim held.
//!
//! A `dbgctl reload wifid` kills the old driver with the chip up; that needs
//! nothing here: `rtw_mac_power_on`'s translation finds the MAC already on
//! (`CR` reads 0xea) and cycles it off and on, as upstream does.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rtw88::dev::Bw;
use rtw88::pci::Pci;
use rtw88::Dev;

use super::bus::{Bar, Dma};
use super::survey::{self, Survey};
use super::{chip, trace};

/// What the chip library is given: the card's registers, DMA memory and a
/// delay. Built once the card is claimed and mapped.
pub(super) struct Bus {
    pub(super) regs: Bar,
    pub(super) dma: Dma,
    pub(super) delay_us: fn(u32),
}

/// A bring-up step that failed, for `WIFID:FAIL:<step>`.
pub(super) struct Failure {
    pub(super) step: &'static str,
    pub(super) detail: String,
}

fn chip_failure(step: &'static str, error: rtw88::Error) -> Failure {
    Failure {
        step,
        detail: format!("{error:?}"),
    }
}

/// The chip, as the library drives it.
pub(super) type Chip = Dev<Pci<Bar, Dma>>;

/// RX buffers handled per poll before looking at the clock again.
const RX_BUDGET: u32 = 64;

/// The radio for the serve loop.
pub(super) struct Radio {
    pub(super) dev: Chip,
    mac: [u8; 6],
    /// Interrupt messages received.
    pub(super) interrupts: u64,
    survey: Survey,
    /// Index into [`survey::CHANNELS`] of the channel being surveyed, `None`
    /// while resting between sweeps.
    hop: Option<usize>,
    /// When to hop next (100 Hz ticks).
    next_tick: u64,
}

/// Power the radio up; on failure the registers (a second handle) come
/// back for the dump. The caller serves it (`hardware.rs`) or surveys it
/// ([`Radio::survey`]).
pub(super) fn bring_up(bus: Bus, firmware: Vec<u8>) -> Result<(Chip, [u8; 6]), (Bar, Failure)> {
    let dump = bus.regs.clone();
    (bus.delay_us)(10);
    let pci = Pci::new(bus.regs, bus.dma).map_err(|e| (dump.clone(), chip_failure("pci", e)))?;
    trace::event("DMA", &[("bytes", &pci.layout().total())]);
    let mut dev = Dev::new(pci);
    match start(&mut dev, &firmware) {
        Ok(mac) => Ok((dev, mac)),
        Err(why) => Err((dump, why)),
    }
}

/// Upstream's order from probe to a started station port.
fn start(dev: &mut Chip, firmware: &[u8]) -> Result<[u8; 6], Failure> {
    dev.chip_info_setup(firmware)
        .map_err(|e| chip_failure("chip-info", e))?;
    report_chip(dev);
    let (cut, rfe) = (dev.hal.cut_version, dev.efuse.rfe_option);
    dev.hci.phy_cfg(cut, rfe).map_err(|e| chip_failure("pci-phy", e))?;
    dev.core_start(firmware)
        .map_err(|e| chip_failure("core-start", e))?;
    let fw = &dev.fw;
    trace::event(
        "FW:READY",
        &[
            ("version", &format!("{}.{}.{}", fw.version, fw.sub_version, fw.sub_index).as_str()),
            ("h2c", &fw.h2c_version),
            ("feature", &trace::Hex32(fw.feature)),
            ("iqk", &dev.dm.iqk_done),
            ("dpk_path_ok", &dev.dm.dpk.path_ok),
        ],
    );
    report_coex(dev);
    let mac = dev.mac_addr();
    dev.add_station(mac).map_err(|e| chip_failure("station", e))?;
    trace::event("MAC", &[("addr", &trace::Mac(mac))]);
    Ok(mac)
}

fn report_chip(dev: &Chip) {
    let (hal, efuse) = (&dev.hal, &dev.efuse);
    trace::event(
        "CHIP:INFO",
        &[
            ("cut", &hal.cut_version),
            ("rf_paths", &hal.rf_path_num),
            ("rfe", &efuse.rfe_option),
            ("btcoex", &efuse.btcoex),
            ("share_ant", &efuse.share_ant),
            ("crystal", &efuse.crystal_cap),
            ("efuse_mac", &trace::Mac(efuse.addr)),
        ],
    );
}

fn report_coex(dev: &Chip) {
    let Some(grant) = dev.coex.grant else { return };
    let lte = grant.lte_ctrl.map(trace::Hex32);
    trace::event(
        "COEX",
        &[
            ("wifi_owns_antenna", &grant.ok()),
            ("lte_ctrl", &OptHex(lte)),
            ("path_wifi", &grant.path_is_wifi),
            ("indirect_errors", &grant.indirect_errors),
        ],
    );
}

/// `none` or the value.
struct OptHex(Option<trace::Hex32>);

impl core::fmt::Display for OptHex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.0 {
            Some(v) => v.fmt(f),
            None => f.write_str("none"),
        }
    }
}

impl Radio {
    /// Survey with a started chip (`mode=survey`), starting a sweep now.
    pub(super) fn survey(dev: Chip, mac: [u8; 6], now: u64) -> Radio {
        let mut radio = Radio {
            dev,
            mac,
            interrupts: 0,
            survey: Survey::default(),
            hop: None,
            next_tick: now,
        };
        radio.start_sweep(now);
        radio
    }

    /// The next tick the serve loop must wake at.
    pub(super) fn deadline(&self) -> u64 {
        self.next_tick
    }

    /// One interrupt message arrived: read and clear the causes, drain RX.
    pub(super) fn interrupt(&mut self) {
        self.interrupts += 1;
        let events = self.dev.hci.interrupt();
        if self.interrupts == 1 {
            trace::event(
                "IRQ:FIRST",
                &[
                    ("hisr0", &trace::Hex32(events.status[0])),
                    ("hisr1", &trace::Hex32(events.status[1])),
                    ("hisr3", &trace::Hex32(events.status[3])),
                ],
            );
        }
        if events.rx {
            self.drain_rx();
        }
    }

    /// Hand every waiting RX buffer to the survey (C2H packets to the chip
    /// library), polling again while `rx_poll` says so.
    fn drain_rx(&mut self) {
        loop {
            let mut c2h: Vec<Vec<u8>> = Vec::new();
            let survey = &mut self.survey;
            let poll = self.dev.hci.rx_poll(RX_BUDGET, |p| {
                if p.stat.is_c2h {
                    survey.c2h();
                    c2h.push(p.payload().to_vec());
                } else {
                    survey.frame(p.payload(), p.stat.crc_err, p.stat.rx_power[0]);
                }
            });
            for pkt in c2h {
                let _ = self.dev.handle_c2h(&pkt);
            }
            if !poll.more {
                break;
            }
        }
    }

    /// Called on every wake: drain RX if anything waits (an interrupt may
    /// have been missed), and hop channels on time.
    pub(super) fn tick(&mut self, now: u64) {
        if self.dev.hci.rx_pending() != 0 {
            self.drain_rx();
        }
        if now < self.next_tick {
            return;
        }
        match self.hop {
            None => self.start_sweep(now),
            Some(i) => {
                self.survey.report_channel();
                self.visit(i + 1, now);
            }
        }
    }

    fn start_sweep(&mut self, now: u64) {
        if let Err(e) = self.dev.scan_start(self.mac) {
            trace::fail("scan-start", format_args!("{e:?}"));
        }
        self.visit(0, now);
    }

    /// Tune to `CHANNELS[i]`, or end the sweep past the last one.
    fn visit(&mut self, i: usize, now: u64) {
        let Some(&channel) = survey::CHANNELS.get(i) else {
            self.survey.report_sweep();
            if let Err(e) = self.dev.scan_complete() {
                trace::fail("scan-complete", format_args!("{e:?}"));
            }
            self.hop = None;
            self.next_tick = now + survey::REST_TICKS;
            return;
        };
        if let Err(e) = self.dev.set_channel(channel, Bw::Mhz20, channel) {
            trace::fail("set-channel", format_args!("ch={channel} {e:?}"));
        }
        self.survey.enter(channel);
        self.hop = Some(i);
        self.next_tick = now + survey::DWELL_TICKS;
    }
}

/// The register dump printed after a failed bring-up step: the always-on
/// system registers and the MAC's.
pub(super) fn failure_dump(regs: &Bar) {
    chip::dump("fail", regs, chip::SYSTEM_REGS);
    chip::dump("fail", regs, chip::MAC_REGS);
}

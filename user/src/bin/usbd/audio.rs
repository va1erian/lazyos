//! The USB audio class (docs/usb-audio-plan.md): a speaker, headset or DAC
//! bound to its AudioControl interface, its playback stream driven for
//! `sndd usb=1` over `os.lazy.usb.audio.v1` (`audio_service.rs`).
//!
//! [`AudioFn::bind`] parses the function from the raw configuration chain
//! (`libs/usbaudio`), asks a UAC 2.0 clock for its rates and reads the
//! product string; nothing is switched on yet. [`AudioFn::configure`]
//! chooses the alternate and rate, adds its endpoints with Configure
//! Endpoint (a smaller alternate after a Bandwidth Error), sends
//! SET_INTERFACE, sets the rate and unmutes the feature unit (`audio_setup.rs`);
//! [`AudioFn::pump`] keeps the isochronous ring fed (`audio_ring.rs`).
//!
//! One audio device at a time across every controller: a second is
//! reported (`USBD:AUDIO:BUSY`) and left alone.
//!
//! Serial evidence: `USBD:AUDIO:BIND`, `USBD:AUDIO:BUSY`, `USBD:AUDIO:UP`
//! (configured: alternate, rate, format, pace), `USBD:AUDIO:RATE` (what the
//! device reads back), `USBD:AUDIO:START`/`STOP`, `USBD:AUDIO:FAIL`,
//! `USBD:AUDIO:XFER` (a TD the controller failed).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use user::trace;

use usbaudio::choose::{choose, function_rates, smaller};
use usbaudio::desc::{parse_functions, Alt, Function, Version};
use usbhid::desc::Interface;
use user::messenger::usb_audio::wire;
use user::sys;
use xhci::regs::Speed;
use xhci::trb::Trb;

use super::audio_ring::{IsoStream, Periods, Staging};
use super::device::Device;
use super::hc::Hc;
use super::mem::Region;
use super::Error;

#[path = "audio_setup.rs"]
mod setup;

/// An audio function is bound somewhere (one at a time).
static BOUND: AtomicBool = AtomicBool::new(false);

/// What a stream call failed with, as the service's errno.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Out of order or a bad range (`EINVAL`).
    Invalid,
    /// No alternate fits or the bus has no room (`ENOSPC`).
    NoRoom,
    /// The device refused or failed (`EIO`).
    Device,
}

/// The configured stream.
struct Live {
    alt: usize,
    rate: u32,
    data_dci: u8,
    feedback_dci: Option<u8>,
    iso: IsoStream,
    running: bool,
}

pub(super) struct AudioFn {
    function: Function,
    name: String,
    port: String,
    vendor: u16,
    product: u16,
    high_speed: bool,
    region: Option<Region>,
    staging: Option<Staging>,
    periods: Periods,
    live: Option<Live>,
}

impl AudioFn {
    /// The playback function on AudioControl `interface`, if it has one and
    /// no other audio device is bound.
    pub(super) fn bind(
        hc: &mut Hc,
        device: &mut Device,
        interface: &Interface,
    ) -> Result<Option<AudioFn>, Error> {
        let Some(mut function) = parse_functions(&device.config_bytes)
            .ok()
            .and_then(|all| all.into_iter().find(|f| f.control_interface == interface.number))
            .filter(|f| !f.alts.is_empty())
        else {
            return Ok(None);
        };
        if matches!(device.at.speed, Speed::Super | Speed::SuperPlus) {
            // Burst and payload come from the SuperSpeed companion, which the
            // class parser does not read: no UAC 3 / SuperSpeed audio yet.
            trace::line!(trace::Channel::Usbd, "AUDIO:SKIP port={} SuperSpeed audio is not supported", device.name);
            return Ok(None);
        }
        if BOUND.swap(true, Ordering::AcqRel) {
            trace::line!(trace::Channel::Usbd, "AUDIO:BUSY port={} another audio device is in use", device.name);
            return Ok(None);
        }
        if function.version == Version::Uac2 {
            setup::read_clock_rates(hc, device, &mut function);
        }
        let region = match hc.take_audio_region() {
            Ok(region) => region,
            Err(error) => {
                BOUND.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let name = device.product_name(hc);
        let high_speed = device.at.speed == Speed::High;
        trace::line!(trace::Channel::Usbd, "AUDIO:BIND port={} slot={} vendor={:#06x} product={:#06x} uac={} alts={} rates={:#x} high_speed={high_speed} name={name}",
            device.name,
            device.slot,
            device.descriptor.vendor,
            device.descriptor.product,
            if function.version == Version::Uac2 { 2 } else { 1 },
            function.alts.len(),
            function_rates(&function));
        Ok(Some(AudioFn {
            function,
            name,
            port: device.name.clone(),
            vendor: device.descriptor.vendor,
            product: device.descriptor.product,
            high_speed,
            region: Some(region),
            staging: None,
            periods: Periods::default(),
            live: None,
        }))
    }

    /// Whether transfer events of `dci` are this stream's.
    pub(super) fn owns(&self, dci: u8) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.data_dci == dci || live.feedback_dci == Some(dci))
    }

    /// The device, for `Describe`.
    pub(super) fn describe(&self) -> wire::Device {
        let preferred = choose(&self.function, 48_000, &[]).map(|c| &self.function.alts[c.alt]);
        wire::Device {
            vendor: u32::from(self.vendor),
            product: u32::from(self.product),
            name: self.name.clone(),
            version: if self.function.version == Version::Uac2 { 2 } else { 1 },
            rates: function_rates(&self.function),
            channels: preferred.map_or(0, |alt| u32::from(alt.format.channels)),
            bits: preferred.map_or(0, |alt| u32::from(alt.format.bits)),
            sync: preferred.map_or(0, |alt| alt.data.sync() as u32),
            high_speed: self.high_speed,
            port: self.port.clone(),
        }
    }

    /// `Attach`: the staging buffer periods come from (a new one stops the
    /// stream: its periods named the old one).
    pub(super) fn attach(&mut self, hc: &mut Hc, device: &mut Device, staging: Staging) {
        self.stop(hc, device);
        if let Some(old) = self.staging.replace(staging) {
            let _ = sys::buffer_close(old.handle);
        }
    }

    /// `Configure`: the alternate and rate for a stereo stream at `rate`.
    pub(super) fn configure(
        &mut self,
        hc: &mut Hc,
        device: &mut Device,
        rate: u32,
    ) -> Result<u32, Refusal> {
        self.stop(hc, device);
        let choice = choose(&self.function, rate, &[]).ok_or(Refusal::NoRoom)?;
        let mut refused: Vec<usize> = Vec::new();
        let mut alt = choice.alt;
        // The previous alternate's endpoints are dropped in the same
        // command; a failure switches them off, so the controller never keeps
        // endpoints (and bandwidth) the stream has forgotten.
        let previous = self.live.take().map(|l| {
            let dcis: Vec<u8> = [Some(l.data_dci), l.feedback_dci].into_iter().flatten().collect();
            (self.function.alts[l.alt].interface, dcis)
        });
        let old: Vec<u8> = previous.as_ref().map(|(_, dcis)| dcis.clone()).unwrap_or_default();
        let region = self.region.as_mut().ok_or(Refusal::Device)?;
        let outcome = loop {
            match setup::switch_on(hc, device, region, &self.function, alt, choice.rate, &old) {
                Ok(live) => break Ok(live),
                Err(setup::Failure::Bandwidth) => {
                    refused.push(alt);
                    match smaller(&self.function, alt, choice.rate, &refused) {
                        Some(next) => alt = next,
                        None => {
                            fail(device, "no alternate fits the bus bandwidth");
                            break Err(Refusal::NoRoom);
                        }
                    }
                }
                Err(setup::Failure::Other(error)) => {
                    fail(device, &alloc::format!("{error}"));
                    break Err(Refusal::Device);
                }
            }
        };
        let live = match outcome {
            Ok(live) => live,
            Err(refusal) => {
                if let Some((interface, dcis)) = previous {
                    setup::switch_off(hc, device, interface, &dcis);
                }
                return Err(refusal);
            }
        };
        let (data_dci, feedback_dci, iso) = live;
        self.periods = Periods::default();
        let picked: &Alt = &self.function.alts[alt];
        trace::line!(trace::Channel::Usbd, "AUDIO:UP port={} interface={} alt={} rate={} channels={} subslot={} bits={} sync={:?} feedback={} packet={} lead={}",
            device.name,
            picked.interface,
            picked.alternate,
            choice.rate,
            picked.format.channels,
            picked.format.subslot,
            picked.format.bits,
            picked.data.sync(),
            feedback_dci.is_some(),
            picked.data.max_esit(),
            iso.lead());
        self.live = Some(Live {
            alt,
            rate: choice.rate,
            data_dci,
            feedback_dci,
            iso,
            running: false,
        });
        Ok(choice.rate)
    }

    /// `Start`: send from now on (silence until periods arrive).
    pub(super) fn start(&mut self, hc: &mut Hc, device: &mut Device) -> Result<(), Refusal> {
        let live = self.live.as_mut().ok_or(Refusal::Invalid)?;
        if !live.running {
            live.running = true;
            trace::line!(trace::Channel::Usbd, "AUDIO:START port={} rate={}", device.name, live.rate);
        }
        self.pump(hc, device);
        Ok(())
    }

    /// `Stop`: stop the endpoints; every queued period counts as played.
    pub(super) fn stop(&mut self, hc: &mut Hc, device: &mut Device) {
        self.periods.flush();
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if !live.running {
            return;
        }
        live.running = false;
        let (data, feedback) = live.iso.abandon();
        let _ = device.recover(hc, live.data_dci, data);
        if let (Some(dci), Some(pointer)) = (live.feedback_dci, feedback) {
            let _ = device.recover(hc, dci, pointer);
        }
        trace::line!(trace::Channel::Usbd, "AUDIO:STOP port={} underruns={} missed={} errors={} pace={}",
            device.name, live.iso.underruns, live.iso.missed, live.iso.errors, live.iso.rate_hz());
    }

    /// `Release`: stop, and the zero-bandwidth alternate.
    pub(super) fn release(&mut self, hc: &mut Hc, device: &mut Device) {
        self.stop(hc, device);
        if let Some(live) = self.live.take() {
            let interface = self.function.alts[live.alt].interface;
            let dcis: Vec<u8> = [Some(live.data_dci), live.feedback_dci].into_iter().flatten().collect();
            setup::switch_off(hc, device, interface, &dcis);
        }
    }

    /// `Submit`: one period of the staging buffer.
    pub(super) fn submit(&mut self, offset: u32, len: u32) -> Result<(), Refusal> {
        let staging = self.staging.as_ref().ok_or(Refusal::Invalid)?;
        let (offset, len) = (offset as usize, len as usize);
        let fits = offset.checked_add(len).is_some_and(|end| end <= staging.len());
        if self.live.is_none() || len == 0 || !fits || len % usbaudio::convert::SOURCE_FRAME != 0 {
            return Err(Refusal::Invalid);
        }
        self.periods.push(offset, len);
        Ok(())
    }

    /// `Reap`.
    pub(super) fn progress(&self) -> wire::Progress {
        let iso = self.live.as_ref().map(|live| &live.iso);
        wire::Progress {
            completed: self.periods.completed,
            underruns: iso.map_or(0, |iso| iso.underruns),
            missed: iso.map_or(0, |iso| iso.missed),
        }
    }

    /// Keep a running stream's rings fed; whether a TD was queued.
    pub(super) fn pump(&mut self, hc: &mut Hc, device: &mut Device) -> bool {
        let (Some(live), Some(region)) = (self.live.as_mut(), self.region.as_mut()) else {
            return false;
        };
        if !live.running {
            return false;
        }
        let filled = live.iso.fill(region, &mut self.periods, self.staging.as_ref());
        let added = match filled {
            Ok(added) => added,
            Err(error) => {
                // Stopped for real (endpoints stopped, rings skipped), so the
                // next Configure never clears memory under a running endpoint.
                fail(device, &alloc::format!("fill: {error}"));
                self.stop(hc, device);
                return false;
            }
        };
        if added {
            hc.doorbell(device.slot, live.data_dci);
        }
        if let (Some(dci), Ok(true)) = (live.feedback_dci, live.iso.fill_feedback(region)) {
            hc.doorbell(device.slot, dci);
        }
        added
    }

    /// A transfer event of one of this stream's endpoints.
    pub(super) fn on_transfer(&mut self, hc: &mut Hc, device: &mut Device, event: &Trb) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if event.endpoint() == live.data_dci {
            live.iso.complete(event, &mut self.periods);
        } else if let Some(region) = self.region.as_ref() {
            live.iso.feedback_report(event, region);
        }
        self.pump(hc, device);
    }

    /// The device is going away: its memory and the staging go back.
    pub(super) fn close(mut self, hc: &mut Hc) {
        if let Some(region) = self.region.take() {
            hc.give_audio_region(region);
        }
        if let Some(staging) = self.staging.take() {
            let _ = sys::buffer_close(staging.handle);
        }
        BOUND.store(false, Ordering::Release);
    }
}

fn fail(device: &Device, why: &str) {
    trace::line!(trace::Channel::Usbd, "AUDIO:FAIL port={} {why}", device.name);
}

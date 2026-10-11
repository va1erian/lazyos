//! Scanning control: `Scan`, `AbortScan`, `ScanResults`, and the channel
//! stepping [`SoftMac::poll`] drives (the plan and the results are
//! `scan.rs`).

use alloc::vec::Vec;

use crate::events::Event;
use crate::mac::SoftMac;
use crate::radio::{Band, Channel, Radio};
use crate::regdom::{Domain, Use};
use crate::scan::{self, Page, Step};
use crate::Error;

/// `Scan`'s request, as the wire carries it (validated here).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanRequest {
    pub channels: Vec<u32>,
    /// A `Band` ordinal, or `u32::MAX` for every band.
    pub band: u32,
    pub ssids: Vec<Vec<u8>>,
    pub active: bool,
    pub dwell_ms: u32,
}

impl<R: Radio> SoftMac<R> {
    /// The band of a requested channel: the named band, or the channel
    /// number's own with `u32::MAX`; it must be one the radio has.
    pub(crate) fn band_of(&self, band: u32, number: u32) -> Result<Band, Error> {
        let band = if band == u32::MAX {
            Band::of_channel(number)
        } else {
            Band::from_u32(band)
        };
        band.filter(|b| self.info.bands & b.bit() != 0)
            .ok_or(Error::Invalid)
    }

    fn plan(&self, request: &ScanRequest, domain: Domain) -> Result<Vec<Step>, Error> {
        let mut channels = Vec::new();
        if request.channels.is_empty() {
            for band in [Band::Ghz2, Band::Ghz5, Band::Ghz6] {
                let wanted = request.band == u32::MAX || request.band == band as u32;
                if wanted && self.info.bands & band.bit() != 0 {
                    channels.extend(domain.channels(band));
                }
            }
        } else {
            if request.channels.len() > scan::MAX_SCAN_CHANNELS {
                return Err(Error::Invalid);
            }
            for &number in &request.channels {
                let band = self.band_of(request.band, number)?;
                let number = u8::try_from(number).map_err(|_| Error::Invalid)?;
                domain.allows(band, number).ok_or(Error::Invalid)?;
                channels.push(Channel::new(band, number));
            }
        }
        if channels.is_empty() {
            return Err(Error::Invalid);
        }
        let dwell = |probe| match request.dwell_ms {
            0 if probe => scan::DWELL_ACTIVE_MS,
            0 => scan::DWELL_PASSIVE_MS,
            ms => ms.clamp(scan::DWELL_MIN_MS, scan::DWELL_MAX_MS),
        };
        Ok(channels
            .into_iter()
            .map(|channel| {
                let probe = request.active
                    && domain.allows(channel.band, channel.number) == Some(Use::Active);
                Step {
                    channel,
                    probe,
                    dwell_ms: dwell(probe),
                }
            })
            .collect())
    }

    /// `Scan`: start stepping channels; returns the scan id at once.
    pub fn scan(&mut self, caller: u64, request: &ScanRequest, now: u64) -> Result<u32, Error> {
        self.owned(caller)?;
        if self.scanner.running.is_some() {
            return Err(Error::Busy);
        }
        if request.active && self.domain.is_none() {
            return Err(Error::Again);
        }
        let too_long = request.ssids.iter().any(|s| s.len() > scan::MAX_SSID);
        if request.ssids.len() > scan::MAX_SCAN_SSIDS || too_long {
            return Err(Error::Invalid);
        }
        let steps = self.plan(request, self.domain.unwrap_or(Domain::WORLD))?;
        self.radio
            .set_scan_filter(true)
            .map_err(|e| self.radio_failed(e))?;
        self.stats.scans += 1;
        let id = self.scanner.start(steps, request.ssids.clone());
        self.scan_step(now);
        Ok(id)
    }

    /// `AbortScan`: not an error when that scan is not running.
    pub fn abort_scan(&mut self, caller: u64, scan_id: u32, now: u64) -> Result<(), Error> {
        self.owned(caller)?;
        if self.scanner.running_id() == Some(scan_id) {
            self.finish_scan(true, now);
        }
        Ok(())
    }

    /// `ScanResults`: a page of at most `max` entries, within `byte_budget`
    /// octets of frames (what one reply can carry).
    pub fn scan_results(
        &self,
        caller: u64,
        scan_id: u32,
        first: u32,
        max: u32,
        byte_budget: usize,
        now: u64,
    ) -> Result<Page<'_>, Error> {
        self.owned(caller)?;
        self.scanner
            .page(scan_id, first, max, byte_budget, now)
            .ok_or(Error::NotFound)
    }

    /// Move to the next channel of the running scan, or finish it.
    pub(crate) fn scan_step(&mut self, now: u64) {
        let Some(run) = self.scanner.running.as_mut() else {
            return;
        };
        let next = run.index.map_or(0, |i| i + 1);
        let Some(&step) = run.steps.get(next) else {
            self.finish_scan(false, now);
            return;
        };
        run.index = Some(next);
        run.until_ms = now + u64::from(step.dwell_ms);
        let ssids = run.ssids.clone();
        if let Err(error) = self.radio.tune(step.channel) {
            // A channel the radio cannot tune is skipped at the next poll.
            self.radio_failed(error);
            return;
        }
        if step.probe {
            self.send_probes(step.channel, &ssids);
        }
    }

    /// End the running scan: back to the joined BSS's channel, and tell the
    /// owner.
    pub(crate) fn finish_scan(&mut self, aborted: bool, now: u64) {
        let Some(run) = self.scanner.running.take() else {
            return;
        };
        let _ = self.radio.set_scan_filter(false);
        if let Some(bss) = self.bss.as_mut() {
            // Beacons could not be heard off the channel: start counting again.
            bss.last_beacon = now;
            bss.loss_reported = false;
            let channel = bss.channel;
            if let Err(error) = self.radio.tune(channel) {
                self.radio_failed(error);
            }
        }
        self.emit(Event::ScanDone {
            scan_id: run.id,
            aborted,
        });
    }
}

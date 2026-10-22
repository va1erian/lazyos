//! A USB speaker, headset or DAC as a card (`sndd usb=1`,
//! docs/usb-audio-plan.md): the device behind `usbd`, reached over
//! `os.lazy.usb.audio.v1`.
//!
//! `usbd` owns the xHCI controller, so this card touches no hardware: its
//! staging is a shared buffer `usbd` copies periods out of, a submit is a
//! `Submit` call, and completions are the running count `Reap` returns
//! (periods complete in order, so the slots come back in the order they
//! went). The device is always fed interleaved stereo `S16Le`; `usbd`
//! converts to whatever the device takes. Once the link fails (the device
//! was unplugged, `usbd` restarted) the card is [`lost`](UsbCard::lost) and
//! the driver exits for `init` to start it again, waiting for the next device.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use user::trace;

use user::messenger::registry;
use user::messenger::usb_audio::{Client, Device, NAME};
use user::sys;
use virtio_snd::params::RATES;
use virtio_snd::wire::{direction, format, PcmInfo};

use super::card::StreamOp;
use super::dma::Region;
use super::error::Error;

/// Staging shared with `usbd`, as large as the other cards'.
const STAGING_BYTES: usize = 64 * 1024;
/// Ticks between looks for a device while none is bound.
const WAIT_TICKS: u64 = 100;
// `usbd`'s rate bitmap (`usbaudio::STANDARD_RATES`, the `AudioInfo.rates`
// numbering) is read through virtio-sound's table: the two must agree.
const _: () = {
    assert!(usbaudio::STANDARD_RATES.len() == RATES.len());
    let mut index = 0;
    while index < RATES.len() {
        assert!(usbaudio::STANDARD_RATES[index] == RATES[index].0);
        index += 1;
    }
};

/// Ticks between liveness checks of the link while idle.
const ALIVE_TICKS: u64 = 100;

pub(super) struct UsbCard {
    client: Client,
    pub(super) device: Device,
    staging: Option<Region>,
    /// Slots submitted and not yet reported complete, oldest first.
    queued: VecDeque<usize>,
    /// Completed slots not yet handed to the stream.
    done: VecDeque<usize>,
    /// `Progress.completed` already accounted for.
    seen: u64,
    lost: bool,
    /// When the link was last known alive (`service_irq`).
    checked_at: u64,
}

impl UsbCard {
    /// Wait for `usbd` to bind an audio device, then share the staging.
    pub(super) fn open() -> Result<UsbCard, Error> {
        let mut logged = false;
        let client = loop {
            match served_by_usbd().then(Client::connect) {
                Some(Ok(client)) => break client,
                _ => {
                    if !logged {
                        trace::line!(trace::Channel::Sndd, "USB:WAIT no USB audio device yet");
                        logged = true;
                    }
                    sys::wait(sys::clock() + WAIT_TICKS);
                }
            }
        };
        let device = client.describe().map_err(link)?;
        let staging = Region::shared(STAGING_BYTES)?;
        client
            .attach(staging.handle(), staging.len() as u64)
            .map_err(link)?;
        trace::line!(trace::Channel::Sndd, "USB:CARD port={} vendor={:#06x} product={:#06x} uac={} rates={:#x} channels={} bits={} name={}",
            device.port, device.vendor, device.product, device.version, device.rates, device.channels, device.bits, device.name);
        Ok(UsbCard {
            client,
            device,
            staging: Some(staging),
            queued: VecDeque::new(),
            done: VecDeque::new(),
            seen: 0,
            lost: false,
            checked_at: sys::clock(),
        })
    }

    /// What to show a person for it.
    pub(super) fn display_name(&self) -> String {
        if self.device.name.is_empty() {
            String::from("USB audio")
        } else {
            format!("USB: {}", self.device.name)
        }
    }

    /// The link to `usbd` failed: the device is gone.
    pub(super) fn lost(&self) -> bool {
        self.lost
    }

    fn check<T>(&mut self, result: user::messenger::Result<T>) -> Result<T, Error> {
        result.map_err(|error| {
            self.lost = true;
            link(error)
        })
    }

    /// One playback stream of stereo `S16Le` at every rate the device has.
    pub(super) fn pcm_infos(&mut self) -> Result<Vec<PcmInfo>, Error> {
        let rates = RATES
            .iter()
            .enumerate()
            .filter(|(bit, _)| self.device.rates >> bit & 1 == 1)
            .fold(0u64, |mask, (_, &(_, code))| mask | 1 << code);
        Ok(vec![PcmInfo {
            formats: 1 << format::S16,
            rates,
            direction: direction::OUTPUT,
            channels_min: 2,
            channels_max: 2,
        }])
    }

    pub(super) fn take_staging(&mut self) -> Result<Region, Error> {
        self.staging.take().ok_or(Error::Busy)
    }

    pub(super) fn give_back(&mut self, region: Region) {
        self.staging = Some(region);
    }

    /// Switch the device to the stream's rate (the format is always stereo
    /// `S16Le`; the period size is the card model's alone).
    pub(super) fn set_params(
        &mut self,
        _stream: u32,
        _buffer_bytes: u32,
        _period_bytes: u32,
        _channels: u8,
        _format: u8,
        rate: u8,
    ) -> Result<(), Error> {
        let hz = RATES
            .iter()
            .find(|&&(_, code)| code == rate)
            .map(|&(hz, _)| hz)
            .ok_or(Error::Params)?;
        let granted = self.client.configure(hz);
        let granted = self.check(granted)?;
        if granted != hz {
            trace::line!(trace::Channel::Sndd, "USB:RATE asked={hz} granted={granted}");
            return Err(Error::Unsupported);
        }
        self.queued.clear();
        self.done.clear();
        self.seen = 0;
        Ok(())
    }

    pub(super) fn stream_op(&mut self, op: StreamOp, _stream: u32) -> Result<(), Error> {
        let result = match op {
            StreamOp::Prepare => return Ok(()),
            StreamOp::Start => self.client.start(),
            StreamOp::Stop => self.client.stop(),
            StreamOp::Release => self.client.release(),
        };
        self.check(result)
    }

    pub(super) fn submit(
        &mut self,
        _stream: u32,
        _ring: &Region,
        slot: usize,
        period_bytes: usize,
        len: usize,
    ) -> Result<(), Error> {
        let offset = slot.checked_mul(period_bytes).ok_or(Error::Range)?;
        let (offset, len) = (
            u32::try_from(offset).map_err(|_| Error::Range)?,
            u32::try_from(len).map_err(|_| Error::Range)?,
        );
        let result = self.client.submit(slot as u32, offset, len);
        self.check(result)?;
        self.queued.push_back(slot);
        Ok(())
    }

    /// The next completed slot, asking `usbd` when none is known yet.
    pub(super) fn reap(&mut self) -> Result<Option<(usize, bool)>, Error> {
        if self.done.is_empty() && !self.queued.is_empty() {
            let progress = self.client.reap();
            let progress = self.check(progress)?;
            let fresh = progress.completed.saturating_sub(self.seen);
            for _ in 0..fresh {
                match self.queued.pop_front() {
                    Some(slot) => self.done.push_back(slot),
                    None => break,
                }
            }
            self.seen = progress.completed;
        }
        Ok(self.done.pop_front().map(|slot| (slot, true)))
    }

    /// No interrupt of its own: one tick.
    pub(super) fn wait_event(&mut self) {
        sys::nap();
    }

    /// Called on every wake: while nothing plays nobody else calls `usbd`,
    /// so a device unplugged while idle is noticed here, once a second.
    pub(super) fn service_irq(&mut self) {
        let now = sys::clock();
        if self.lost || now < self.checked_at + ALIVE_TICKS {
            return;
        }
        self.checked_at = now;
        let alive = self.client.reap();
        let _ = self.check(alive);
    }

    pub(super) fn irq_report(&self) -> (bool, u64) {
        (false, 0)
    }
}

/// Whether `os.lazy.usb.audio` is registered by `usbd` (`_usb`, or root on
/// a kernel-spawned boot): the staging buffer, all the system's sound, is
/// shared only with the USB driver.
fn served_by_usbd() -> bool {
    registry::list().is_ok_and(|entries| {
        entries.iter().any(|entry| {
            entry.name == NAME
                && (entry.owner_uid == u64::from(usbpolicy::USB_UID) || entry.owner_uid == 0)
        })
    })
}

/// After the device failed us: wait for `usbd` to withdraw it (unplugged)
/// before exiting, so a device that fails at once every time costs one
/// failure, not a restart storm that makes `init` give up on the row.
pub(super) fn wait_until_gone() {
    while served_by_usbd() {
        sys::wait(sys::clock() + WAIT_TICKS);
    }
}

fn link(error: user::messenger::Error) -> Error {
    Error::Usb(error.message())
}

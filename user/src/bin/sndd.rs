//! `sndd` (`/system/bin/sndd`): the virtio-sound userspace driver
//! (`docs/driver-plan.md`, stage D6).
//!
//! The driver is an ordinary ring-3 program. It claims the virtio-sound PCI
//! function through the device syscall (23), maps its BARs, allocates DMA
//! memory for the virtqueues and the sample slots, and drives the device with
//! the transport in `libs/virtio` and the protocol in `libs/virtio-snd`. It
//! arms the device interrupt when it can and polls otherwise. It serves
//! `os.lazy.audio.v1` (`idl/audio.midl`) under [`api::CARD_NAME`]: a client
//! opens the stream, shares a ring, and the driver copies committed periods
//! into its own DMA slots. That client is the system mixer, `audiod`
//! (docs/audio-plan.md), which holds the one stream for as long as it runs;
//! applications talk to the mixer, never to the card.
//!
//! Boot evidence (`demo=1`, optional `freq=<Hz>` `ms=<milliseconds>`):
//!
//! 1. `SNDD:CARD` and one `SNDD:PCM` per stream describe the device;
//! 2. the driver plays a test tone *directly* and prints `SND:PLAY:PASS` once
//!    the device reported every period consumed;
//! 3. it registers the card (`SNDD:READY`); `audiod demo=1` then runs the
//!    evidence clients through the mixer.
//!
//! The host side (`tools/sound/run.py`) records the audio with QEMU's `wav`
//! backend and checks the recording, so the markers alone are never the proof.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::panic::PanicInfo;

use pcm::tone::Tone;
use user::messenger::audio::{self as api};
use user::messenger::{self, errno, registry, services, Error as MsgError};
use user::sys;
use virtio_snd::params::{audio_direction, audio_format, Request};
use virtio_snd::wire::{direction, PcmInfo};

#[path = "sndd/card.rs"]
mod card;
#[path = "sndd/device.rs"]
mod device;
#[path = "sndd/dma.rs"]
mod dma;
#[path = "sndd/error.rs"]
mod error;
#[path = "sndd/service.rs"]
mod service;
#[path = "sndd/session.rs"]
mod session;
#[path = "sndd/stream.rs"]
mod stream;

use card::Card;
use error::Error;
use service::Service;
use stream::Stream;

/// Defaults for the boot demo: a clearly audible, easily measured tone.
const DEFAULT_FREQ_HZ: u32 = 440;
const DEFAULT_MS: u32 = 800;
const RATE_HZ: u32 = 48000;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: u32 = 8192;
/// Peak level of the tone: about half of full scale, unmistakable and far
/// from clipping.
const AMPLITUDE_Q15: i32 = 16000;

/// Longest park in the serve loop while no stream is running (PIT ticks).
const IDLE_TICKS: u64 = 100;

/// Parsed service arguments.
struct Args {
    demo: bool,
    freq_hz: u32,
    ms: u32,
}

impl Args {
    fn from_service() -> Args {
        let mut buffer = [0u8; 128];
        let len = sys::service_args(&mut buffer).min(buffer.len());
        let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
        let mut args = Args {
            demo: false,
            freq_hz: DEFAULT_FREQ_HZ,
            ms: DEFAULT_MS,
        };
        for part in text.split_whitespace() {
            match part.split_once('=') {
                Some(("demo", "1")) => args.demo = true,
                // Bounded so a hostile argument cannot ask for hours of audio
                // or a frequency the generator would have to clamp.
                Some(("freq", value)) => {
                    args.freq_hz = value.parse().unwrap_or(DEFAULT_FREQ_HZ).clamp(20, 20000)
                }
                Some(("ms", value)) => {
                    args.ms = value.parse().unwrap_or(DEFAULT_MS).clamp(50, 10_000)
                }
                _ => {}
            }
        }
        args
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("sndd: virtio-sound driver\n");
    // The identity the kernel stamped on this task: `_snd` with only
    // `CAP_DEV_CLAIM` under `init`, root when the kernel boots it directly.
    let mut cred = sys::Cred::default();
    match sys::cred_get(None, &mut cred) {
        Ok(()) => sys::write_str(&format!(
            "SNDD:CRED uid={} caps={:#x}\n",
            cred.uid, cred.caps
        )),
        Err(errno) => sys::write_str(&format!("SNDD:CRED unavailable (errno {errno})\n")),
    }
    let args = Args::from_service();
    match run(&args) {
        Ok(()) => sys::exit(0),
        Err(Error::NoDevice) => {
            sys::write_str("SNDD:NODEV no virtio-sound device on this machine\n");
            sys::exit(0)
        }
        Err(error) => {
            sys::write_str(&format!("SND:PLAY:FAIL {}\n", error.describe()));
            sys::exit(1)
        }
    }
}

fn run(args: &Args) -> Result<(), Error> {
    let mut card = Card::open()?;
    sys::write_str(&format!("SNDD:CARD streams={}\n", card.streams));
    let infos = card.pcm_infos()?;
    for (index, info) in infos.iter().enumerate() {
        sys::write_str(&format!(
            "SNDD:PCM stream={index} dir={} formats={:#x} rates={:#x} channels={}-{}\n",
            if info.direction == direction::OUTPUT {
                "out"
            } else {
                "in"
            },
            virtio_snd::params::format_bitmap(info),
            virtio_snd::params::rate_bitmap(info),
            info.channels_min,
            info.channels_max
        ));
    }
    if args.demo {
        // Boot evidence (issue #481): `_snd` cannot claim another class.
        user::dev::inspect::cross_class_probe("snd");
        self_test(&mut card, &infos, args)?;
    }
    serve(card, &infos)
}

/// Play a tone straight through the driver, without a client: the proof the
/// device path works before any Messenger client is involved.
fn self_test(card: &mut Card, infos: &[PcmInfo], args: &Args) -> Result<(), Error> {
    let (index, info) = infos
        .iter()
        .enumerate()
        .find(|(_, info)| info.direction == direction::OUTPUT)
        .ok_or(Error::NoStream)?;
    let request = Request {
        direction: audio_direction::PLAYBACK,
        format: audio_format::S16_LE,
        rate_hz: RATE_HZ,
        channels: CHANNELS,
        period_bytes: PERIOD_BYTES,
    };
    let mut stream = Stream::open(card, index as u32, info, &request)?;
    let grant = stream.grant;
    let frames = u64::from(grant.rate_hz) * u64::from(args.ms) / 1000;
    let mut tone = Tone::new(args.freq_hz, grant.rate_hz, AMPLITUDE_Q15);

    stream.start(card)?;
    let played = stream.play_tone(card, &mut tone, frames)?;
    stream.halt(card)?;
    card.give_back(stream.into_region());
    if played != frames {
        return Err(Error::Params);
    }
    sys::write_str(&format!(
        "SND:PLAY:PASS freq={} rate={} channels={} frames={played}\n",
        args.freq_hz, grant.rate_hz, grant.channels
    ));
    // Interrupt evidence: when the line was armed, the tone's completions must
    // have raised interrupts; an unroutable line just means polling alone.
    match card.irq_report() {
        (false, _) => sys::write_str("SNDD:IRQ:POLLING line not routable, polling only\n"),
        (true, 0) => sys::write_str("SND:IRQ:FAIL armed but no interrupt arrived\n"),
        (true, delivered) => sys::write_str(&format!("SND:IRQ:PASS delivered={delivered}\n")),
    }
    Ok(())
}

/// Register the card's `os.lazy.audio.v1` and serve it for the life of the
/// driver.
fn serve(card: Card, infos: &[PcmInfo]) -> Result<(), Error> {
    let fail = |error: MsgError| Error::Messenger(error.message());
    let (published, server) = messenger::create_pair().map_err(fail)?;
    registry::register(api::CARD_NAME, &published, &[api::INTERFACE], 0).map_err(fail)?;
    sys::write_str(&format!(
        "SNDD:READY name={} interface={:#x}
",
        api::CARD_NAME,
        api::INTERFACE
    ));

    let mut service = Service::new(card, infos);
    // `system/audio/virtio-snd0/event` (issue #453).
    let mut publisher = user::audio_events::EventPublisher::new(user::audio_events::VIRTIO_CARD);
    let mut events = alloc::vec::Vec::new();
    // One receive buffer for the life of the service (the heap never reclaims
    // per-call buffers).
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let park = if service.busy() { 1 } else { IDLE_TICKS };
        match server.recv_with(&mut buffer, Some(sys::clock() + park)) {
            Ok(message) => {
                let reply = service.dispatch(&message).unwrap_or_else(|error| {
                    services::error_reply(message.interface_id(), message.method(), error)
                });
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply).map_err(fail)?;
                }
            }
            Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(fail(error)),
        }
        service.housekeeping(&mut events);
        if !events.is_empty() {
            publisher.publish(&events, sys::clock());
            events.clear();
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}

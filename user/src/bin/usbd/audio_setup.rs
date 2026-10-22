//! Switching an audio device's stream on and off: the class requests and
//! the endpoint configuration around SET_INTERFACE (UAC 1.0 5.2.3.2.3,
//! UAC 2.0 5.2.5.1, xHCI 4.6.6.1).
//!
//! The order is Linux's: Configure Endpoint (drop the old alternate's
//! endpoints, add the new one's), SET_INTERFACE, then the sample rate (UAC
//! 1.0 to the data endpoint when it has the control or the alternate lists
//! several rates; UAC 2.0 to the clock source), read back and logged
//! (`USBD:AUDIO:RATE`), then the feature unit's mute switched off. A device
//! that refuses the rate or the mute is used as it is: many fixed-rate
//! devices stall both.

use alloc::vec;
use alloc::vec::Vec;
use user::trace;

use usbaudio::desc::{Entity, Function, Range, RateSet, Version};
use usbaudio::packet::{esit_microframes, Pacer};
use usbaudio::request::{parse_ranges, parse_rate, range_count, range_reply_len, rate_uac1, rate_uac2};
use xhci::context::{EndpointContext, EndpointType};
use xhci::regs::{self, isoch_interval, Speed};
use xhci::trb::{code, kind, request};

use super::super::audio_ring::{IsoStream, Shape};
use super::super::device::{Device, DATA_BYTES};
use super::super::hc::Hc;
use super::super::mem::Region;
use super::super::Error;

/// Why an alternate could not be switched on.
pub(super) enum Failure {
    /// The controller has no bus time for it: try a smaller one.
    Bandwidth,
    Other(Error),
}

/// What a stream needs once switched on: data DCI, feedback DCI, rings.
pub(super) type Switched = (u8, Option<u8>, IsoStream);

/// UAC 2.0: every clock's rates, from `GET_RANGE`; a clock that will not
/// say gets the rate it runs at now, or 48 kHz.
pub(super) fn read_clock_rates(hc: &mut Hc, device: &mut Device, function: &mut Function) {
    let interface = function.control_interface;
    // `Function::clock_of` follows a selector's first input: make that true
    // on the device too, before its rates are read or set (Linux does the same).
    let selectors: Vec<u8> = function
        .entities
        .iter()
        .filter_map(|entity| match entity {
            Entity::ClockSelector { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    for id in selectors {
        let setup = request::audio::uac2_select_clock(interface, id);
        if let Err(error) = device.control_out_data(hc, setup, &[1]) {
            note(device, "clock selector", &error);
        }
    }
    for clock in function.clocks() {
        let rates = clock_ranges(hc, device, interface, clock).unwrap_or_else(|| {
            let mut now = [0u8; 4];
            let setup = request::audio::uac2_get_rate(interface, clock);
            let rate = device
                .control_in(hc, setup, &mut now)
                .ok()
                .and_then(|()| parse_rate(&now))
                .filter(|&rate| rate != 0)
                .unwrap_or(48_000);
            let mut set = RateSet::default();
            set.push(Range {
                min: rate,
                max: rate,
                step: 0,
            });
            set
        });
        trace::line!(trace::Channel::Usbd, "AUDIO:CLOCK port={} id={clock} ranges={}", device.name, rates.ranges.len());
        function.set_clock_rates(clock, &rates);
    }
}

fn clock_ranges(hc: &mut Hc, device: &mut Device, interface: u8, clock: u8) -> Option<RateSet> {
    let mut head = [0u8; 2];
    let setup = request::audio::uac2_rate_ranges(interface, clock, 2);
    device.control_in(hc, setup, &mut head).ok()?;
    let count = range_count(&head).ok()?;
    let len = range_reply_len(count).min(DATA_BYTES);
    let mut reply = vec![0u8; len];
    let setup = request::audio::uac2_rate_ranges(interface, clock, len as u16);
    device.control_in(hc, setup, &mut reply).ok()?;
    parse_ranges(&reply).ok().filter(|rates| !rates.is_empty())
}

/// Switch alternate `index` of `function` on at `rate`, replacing the
/// endpoints in `old`.
pub(super) fn switch_on(
    hc: &mut Hc,
    device: &mut Device,
    region: &mut Region,
    function: &Function,
    index: usize,
    rate: u32,
    old: &[u8],
) -> Result<Switched, Failure> {
    let alt = &function.alts[index];
    let speed = device.at.speed;
    let high_speed = speed == Speed::High;
    let micro = esit_microframes(high_speed, alt.data.interval);
    let frame = alt.format.frame_bytes().max(1);
    let shape = Shape {
        format: alt.format,
        pacer: Pacer::new(rate, micro, alt.data.max_esit() / frame, high_speed),
        max_packet: alt.data.max_packet,
        transactions: if high_speed { alt.data.transactions } else { 1 },
        esit_us: micro * 125,
        feedback_packet: alt.feedback.map(|feedback| feedback.max_packet),
    };
    let iso = IsoStream::new(region, shape).map_err(Failure::Other)?;
    let data_dci = regs::dci(alt.data.address);
    let burst = if high_speed { alt.data.transactions - 1 } else { 0 };
    let mut add = vec![(
        data_dci,
        EndpointContext::isoch(
            EndpointType::IsochOut,
            alt.data.max_packet,
            burst,
            isoch_interval(speed, alt.data.interval),
            alt.data.max_esit(),
            iso.dequeue(),
        ),
    )];
    let feedback_dci = alt.feedback.map(|feedback| regs::dci(feedback.address));
    if let (Some(feedback), Some(dci), Some(dequeue)) = (alt.feedback, feedback_dci, iso.feedback_dequeue()) {
        // A UAC 1.0 feedback endpoint reports every 2^bRefresh frames.
        let interval = if high_speed {
            feedback.interval
        } else {
            feedback.interval.max(feedback.refresh.saturating_add(1))
        };
        add.push((
            dci,
            EndpointContext::isoch(
                EndpointType::IsochIn,
                feedback.max_packet.min(64),
                0,
                isoch_interval(speed, interval),
                u32::from(feedback.max_packet.min(64)),
                dequeue,
            ),
        ));
    }
    match device.reconfigure(hc, &add, old) {
        Ok(()) => {}
        // Bandwidth Error, or Resource Error (the controller is out of
        // internal resources for an endpoint this size): a smaller one may fit.
        Err(Error::Completion(kind::CONFIGURE_ENDPOINT, code::BANDWIDTH | code::RESOURCE)) => {
            return Err(Failure::Bandwidth)
        }
        Err(error) => return Err(Failure::Other(error)),
    }
    let others: Vec<u8> = add.iter().map(|&(dci, _)| dci).collect();
    device
        .control_out(hc, request::set_interface(alt.interface, alt.alternate))
        .map_err(|error| {
            let _ = device.reconfigure(hc, &[], &others);
            Failure::Other(error)
        })?;
    set_rate(hc, device, function, index, rate);
    if let Some(unit) = function.mute_unit(alt) {
        let setup = request::audio::set_mute(function.control_interface, unit);
        if let Err(error) = device.control_out_data(hc, setup, &[0]) {
            note(device, "unmute", &error);
        }
    }
    Ok((data_dci, feedback_dci, iso))
}

/// Set the alternate's rate where the device lets it be set, and log what
/// it runs at.
fn set_rate(hc: &mut Hc, device: &mut Device, function: &Function, index: usize, rate: u32) {
    let alt = &function.alts[index];
    let (set, get, payload): (_, _, Vec<u8>) = match function.version {
        Version::Uac1 => {
            if !alt.rate_control && alt.rates.ranges.len() < 2 {
                return;
            }
            (
                request::audio::uac1_set_rate(alt.data.address),
                request::audio::uac1_get_rate(alt.data.address),
                rate_uac1(rate).to_vec(),
            )
        }
        Version::Uac2 => {
            let Some(clock) = function.clock_of(alt) else {
                return;
            };
            let interface = function.control_interface;
            (
                request::audio::uac2_set_rate(interface, clock),
                request::audio::uac2_get_rate(interface, clock),
                rate_uac2(rate).to_vec(),
            )
        }
    };
    if let Err(error) = device.control_out_data(hc, set, &payload) {
        note(device, "set rate", &error);
    }
    let mut back = vec![0u8; usize::from(get.length)];
    let read = device
        .control_in(hc, get, &mut back)
        .ok()
        .and_then(|()| parse_rate(&back));
    trace::line!(trace::Channel::Usbd, "AUDIO:RATE port={} asked={rate} device={}", device.name, read.map_or(alloc::string::String::from("?"), |r| alloc::format!("{r}")));
}

/// Switch the stream off: its endpoints dropped, alternate 0.
pub(super) fn switch_off(hc: &mut Hc, device: &mut Device, interface: u8, dcis: &[u8]) {
    if let Err(error) = device.reconfigure(hc, &[], dcis) {
        note(device, "drop endpoints", &error);
    }
    if let Err(error) = device.control_out(hc, request::set_interface(interface, 0)) {
        note(device, "alternate 0", &error);
    }
}

fn note(device: &Device, what: &str, error: &Error) {
    trace::line!(trace::Channel::Usbd, "AUDIO:NOTE port={} {what} refused ({error}), continuing", device.name);
}

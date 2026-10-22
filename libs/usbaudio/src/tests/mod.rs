//! Host tests: three real-world shapes of audio function (QEMU's
//! `usb-audio`, a UAC 1.0 headset with a microphone and a HID button
//! interface, a UAC 2.0 asynchronous DAC), the choice, the pacer, the
//! conversion and the request payloads.

pub(crate) mod fixtures;
mod pacing;

use alloc::vec;
use alloc::vec::Vec;

use crate::choose::{choose, function_rates, smaller};
use crate::convert::{convert, silence};
use crate::desc::{parse_functions, Entity, Format, Sync, Version};
use crate::request::{parse_ranges, parse_rate, rate_uac1, rate_uac2};
use crate::Error;
use fixtures::{config, dac_range_reply, headset, qemu, uac2_dac};

#[test]
fn qemu_usb_audio() {
    let functions = parse_functions(&qemu()).unwrap();
    assert_eq!(functions.len(), 1);
    let function = &functions[0];
    assert_eq!(function.version, Version::Uac1);
    assert_eq!(function.control_interface, 0);
    assert_eq!(function.entities.len(), 3);
    assert_eq!(function.alts.len(), 1);
    let alt = &function.alts[0];
    assert_eq!((alt.interface, alt.alternate, alt.terminal), (1, 1, 1));
    assert_eq!(
        alt.format,
        Format {
            channels: 2,
            subslot: 2,
            bits: 16
        }
    );
    assert!(alt.rates.contains(48000) && !alt.rates.contains(44100));
    assert_eq!(alt.data.address, 0x01);
    assert_eq!(alt.data.sync(), Sync::Synchronous);
    assert_eq!(alt.data.max_esit(), 196);
    assert_eq!(alt.feedback, None);
    assert_eq!(function.mute_unit(alt), Some(2));
    assert_eq!(function.clock_of(alt), None);
    assert_eq!(choose(function, 48000, &[]).unwrap().rate, 48000);
}

#[test]
fn headset_skips_capture_and_keeps_both_rates() {
    let functions = parse_functions(&headset()).unwrap();
    let function = &functions[0];
    assert_eq!(function.alts.len(), 1, "the microphone is not playback");
    let alt = &function.alts[0];
    assert!(alt.rate_control);
    assert!(alt.rates.contains(44100) && alt.rates.contains(48000));
    assert_eq!(function_rates(function), 1 << 5 | 1 << 6);
    // 96 kHz is missing: the closest standard rate it has.
    assert_eq!(choose(function, 96000, &[]).unwrap().rate, 48000);
    assert_eq!(choose(function, 22050, &[]).unwrap().rate, 44100);
}

#[test]
fn uac2_dac_clock_feedback_and_choice() {
    let mut functions = parse_functions(&uac2_dac()).unwrap();
    let function = &mut functions[0];
    assert_eq!(function.version, Version::Uac2);
    assert_eq!(function.alts.len(), 2);
    assert!(function.alts.iter().all(|alt| alt.rates.is_empty()));
    assert_eq!(function.clocks(), vec![0x29]);
    assert!(matches!(
        function.entity(0x29),
        Some(Entity::ClockSource {
            programmable: true,
            ..
        })
    ));
    let rates = parse_ranges(&dac_range_reply()).unwrap();
    function.set_clock_rates(0x29, &rates);
    let first = &function.alts[0];
    assert_eq!(first.format.subslot, 4);
    assert_eq!(first.format.channels, 2);
    assert_eq!(first.data.sync(), Sync::Async);
    assert_eq!(first.feedback.unwrap().address, 0x81);
    assert_eq!(function.mute_unit(first), Some(0x0A));
    // 16 bit beats 24-in-32 at the same rate.
    let choice = choose(function, 48000, &[]).unwrap();
    assert_eq!((choice.alt, choice.rate), (1, 48000));
    assert_eq!(choose(function, 48000, &[1]).unwrap().alt, 0);
    assert_eq!(smaller(function, 0, 48000, &[]), Some(1));
    assert_eq!(smaller(function, 1, 48000, &[]), None);
}

#[test]
fn clock_selector_is_followed_to_its_first_input() {
    let mut bytes = uac2_dac();
    // Point the terminal at a selector (id 0x28) whose first input is the
    // clock source: append the selector to the control interface.
    let selector = [0x08, 0x24, 0x0B, 0x28, 0x01, 0x29, 0x03, 0x00];
    let terminal = bytes
        .windows(4)
        .position(|w| w == [0x11, 0x24, 0x02, 0x02])
        .unwrap();
    bytes[terminal + 7] = 0x28;
    bytes.splice(terminal..terminal, selector);
    let total = (bytes.len() as u16).to_le_bytes();
    bytes[2..4].copy_from_slice(&total);
    let functions = parse_functions(&bytes).unwrap();
    assert_eq!(functions[0].clocks(), vec![0x29]);
}

#[test]
fn hostile_chains() {
    assert_eq!(parse_functions(&[0x01, 0x02]), Err(Error::BadLength));
    assert_eq!(parse_functions(&[0x09, 0x02, 0x00]), Err(Error::BadLength));
    // A clock loop (a selector naming itself) ends without a clock.
    let mut bytes = uac2_dac();
    let terminal = bytes
        .windows(4)
        .position(|w| w == [0x11, 0x24, 0x02, 0x02])
        .unwrap();
    bytes[terminal + 7] = 0x28;
    bytes.splice(terminal..terminal, [0x06, 0x24, 0x0B, 0x28, 0x01, 0x28]);
    let functions = parse_functions(&bytes).unwrap();
    assert!(functions[0].clocks().is_empty());
    // An alternate with an absurd format is dropped, not trusted.
    let mut bytes = qemu();
    let format = bytes
        .windows(4)
        .position(|w| w == [0x0B, 0x24, 0x02, 0x01])
        .unwrap();
    bytes[format + 5] = 9; // a 9-byte subslot
    assert!(parse_functions(&bytes).unwrap()[0].alts.is_empty());
    // A fifth AudioControl interface is not kept, and neither is its
    // streaming interface: it never lands in the fourth function.
    let control = [0x09, 0x04, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00];
    let streaming: [&[u8]; 6] = [
        &[0x09, 0x04, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00],
        &[0x07, 0x24, 0x01, 0x01, 0x01, 0x01, 0x00],
        &[0x0B, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x01, 0x80, 0xBB, 0x00],
        &[0x09, 0x05, 0x01, 0x0D, 0xC4, 0x00, 0x01, 0x00, 0x00],
        &[0x07, 0x25, 0x01, 0x00, 0x00, 0x00, 0x00],
        &[0x05, 0x04, 0x02, 0x00, 0x00],
    ];
    let mut records: Vec<&[u8]> = Vec::new();
    for _ in 0..5 {
        records.push(&control);
        records.extend_from_slice(&streaming);
    }
    let functions = parse_functions(&config(&records)).unwrap();
    assert_eq!(functions.len(), 4);
    assert!(functions.iter().all(|f| f.alts.len() == 1));
    // Not audio at all.
    assert!(parse_functions(&config(&[&[0x09, 0x04, 0, 0, 1, 3, 1, 1, 0]]))
        .unwrap()
        .is_empty());
}

#[test]
fn conversion_layouts() {
    let source = [0x34, 0x12, 0x78, 0x56, 0xFF, 0x7F, 0x00, 0x80];
    let stereo16 = Format {
        channels: 2,
        subslot: 2,
        bits: 16,
    };
    let mut out = [0u8; 64];
    assert_eq!(convert(&source, &stereo16, &mut out), 8);
    assert_eq!(&out[..8], &source);
    let stereo24 = Format {
        channels: 2,
        subslot: 3,
        bits: 24,
    };
    assert_eq!(convert(&source[..4], &stereo24, &mut out), 6);
    assert_eq!(&out[..6], &[0, 0x34, 0x12, 0, 0x78, 0x56]);
    let stereo32 = Format {
        channels: 2,
        subslot: 4,
        bits: 24,
    };
    assert_eq!(convert(&source[..4], &stereo32, &mut out), 8);
    assert_eq!(&out[..8], &[0, 0, 0x34, 0x12, 0, 0, 0x78, 0x56]);
    let mono = Format {
        channels: 1,
        subslot: 2,
        bits: 16,
    };
    // (0x7FFF + -0x8000) / 2 = 0.
    assert_eq!(convert(&source[4..], &mono, &mut out), 2);
    assert_eq!(&out[..2], &[0, 0]);
    let surround = Format {
        channels: 6,
        subslot: 2,
        bits: 16,
    };
    out.fill(0xAA);
    assert_eq!(convert(&source[..4], &surround, &mut out), 12);
    assert_eq!(&out[..12], &[0x34, 0x12, 0x78, 0x56, 0, 0, 0, 0, 0, 0, 0, 0]);
    // The output ends first: whole frames only.
    let mut small = [0u8; 7];
    assert_eq!(convert(&source, &stereo24, &mut small), 6);
    out.fill(0xAA);
    assert_eq!(silence(&stereo24, &mut out[..13]), 12);
    assert!(out[..12].iter().all(|&b| b == 0) && out[12] == 0xAA);
}

#[test]
fn request_payloads() {
    assert_eq!(rate_uac1(48000), [0x80, 0xBB, 0x00]);
    assert_eq!(rate_uac2(44100), 44100u32.to_le_bytes());
    assert_eq!(parse_rate(&[0x44, 0xAC, 0x00]), Some(44100));
    assert_eq!(parse_rate(&[0x80, 0xBB, 0, 0]), Some(48000));
    assert_eq!(parse_rate(&[1]), None);
    let rates = parse_ranges(&dac_range_reply()).unwrap();
    assert!(rates.contains(96000) && !rates.contains(32000));
    // A continuous range with a step.
    let mut reply = vec![1, 0];
    for value in [8000u32, 96000, 4000] {
        reply.extend_from_slice(&value.to_le_bytes());
    }
    let rates = parse_ranges(&reply).unwrap();
    assert!(rates.contains(48000) && !rates.contains(44100));
    // Says two subranges, carries one.
    reply[0] = 2;
    assert_eq!(parse_ranges(&reply), Err(Error::Short));
    assert_eq!(parse_ranges(&[]), Err(Error::Short));
}

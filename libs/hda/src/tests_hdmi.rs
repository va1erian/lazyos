//! `hdmi` against the Kaby Lake HDMI codec as the box's `SNDD:PROBE` lines
//! show it (2026-10-21): converters at 2 and 4, pins at 3, 5, 6 and 7 with
//! the monitor on pin 6, whose connection list names 3, a widget that is a
//! pin and ignores converter verbs.

use std::vec::Vec;

use crate::codec::read_graph;
use crate::fake::{node, qemu_output, wcaps, Codec};
use crate::hdmi::{self, SENSE_ELD_VALID, SENSE_PRESENT};
use crate::verbs::{param, VerbError, Verbs};

struct Direct(Codec);

impl Verbs for Direct {
    fn send(&mut self, command: u32) -> Result<u32, VerbError> {
        Ok(self.0.answer(command))
    }
}

const PIN_CAPS: u32 = 0x0900_0094;
const OUT: u32 = 0x6611;
const PIN: u32 = 0x40_778D;

fn intel(sense: [u32; 4]) -> Codec {
    let mut codec = Codec::default();
    codec
        .nodes
        .insert(0, node(&[(param::NODE_COUNT, 1 << 16 | 1)], &[], 0));
    codec.nodes.insert(
        1,
        node(
            &[(param::FUNCTION_TYPE, 1), (param::NODE_COUNT, 2 << 16 | 2)],
            &[],
            0,
        ),
    );
    for (nid, digital) in [(2u8, 0x80_0001), (4, 0x80_0001)] {
        let mut n = node(&[(param::WIDGET_CAPS, OUT)], &[], 0);
        n.digital = digital;
        codec.nodes.insert(nid, n);
    }
    // (nid, its connection entry, its sense)
    for (nid, conn, sense) in [
        (3u8, 2u16, sense[0]),
        (5, 2, sense[1]),
        (6, 3, sense[2]),
        (7, 4, sense[3]),
    ] {
        let mut n = node(
            &[
                (param::WIDGET_CAPS, PIN | 1 << 8),
                (param::PIN_CAPS, PIN_CAPS),
                (param::CONN_LIST_LEN, 1),
            ],
            &[conn],
            0x1856_0010,
        );
        n.sense = sense;
        codec.nodes.insert(nid, n);
    }
    codec
        .nodes
        .insert(8, node(&[(param::WIDGET_CAPS, wcaps(15, 0))], &[], 0));
    codec
}

const MONITOR: u32 = SENSE_PRESENT | SENSE_ELD_VALID;

fn log(codec: &Codec) -> Vec<(u8, u16, u8)> {
    codec
        .log
        .iter()
        .map(|&(nid, word)| (nid, (word >> 8) as u16, word as u8))
        .collect()
}

#[test]
fn the_pin_with_a_monitor_is_chosen_not_the_first_wired_one() {
    let mut direct = Direct(intel([0, 0, MONITOR, 0]));
    let graph = read_graph(&mut direct, 0).unwrap();
    let found = hdmi::find(&mut direct, 0, &graph).unwrap().unwrap();
    assert_eq!(found.pin, 6);
    assert!(found.eld_valid);
    // 3 is the pin's connection but not a converter; the converters of the
    // group follow.
    assert_eq!(found.converters(), [2, 4]);
    let path = hdmi::path(&found);
    assert_eq!((path.pin(), path.dac(), path.len), (6, 2, 2));
}

#[test]
fn the_group_reports_two_widgets_but_all_seven_are_read() {
    let mut direct = Direct(intel([0, 0, MONITOR, 0]));
    let graph = read_graph(&mut direct, 0).unwrap();
    let nids: Vec<u8> = graph.widgets().map(|w| w.nid).collect();
    assert_eq!(nids, [2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn power_up_wakes_the_function_group() {
    let mut direct = Direct(intel([0, 0, MONITOR, 0]));
    let graph = read_graph(&mut direct, 0).unwrap();
    hdmi::power_up(&mut direct, 0, &graph).unwrap();
    assert_eq!(log(&direct.0), [(1, 0x705, 0)]);
}

#[test]
fn no_monitor_means_no_hdmi_output() {
    let mut direct = Direct(intel([0; 4]));
    let graph = read_graph(&mut direct, 0).unwrap();
    assert_eq!(hdmi::find(&mut direct, 0, &graph).unwrap(), None);
}

#[test]
fn a_valid_eld_beats_presence_alone_whatever_the_order() {
    let mut direct = Direct(intel([SENSE_PRESENT, MONITOR, 0, SENSE_PRESENT]));
    let graph = read_graph(&mut direct, 0).unwrap();
    assert_eq!(hdmi::find(&mut direct, 0, &graph).unwrap().unwrap().pin, 5);
    let mut direct = Direct(intel([0, SENSE_PRESENT, 0, SENSE_PRESENT]));
    let graph = read_graph(&mut direct, 0).unwrap();
    let found = hdmi::find(&mut direct, 0, &graph).unwrap().unwrap();
    assert_eq!(
        (found.pin, found.eld_valid),
        (5, false),
        "first present wins"
    );
}

#[test]
fn an_analog_codec_has_no_hdmi_pin() {
    let mut direct = Direct(qemu_output());
    let graph = read_graph(&mut direct, 0).unwrap();
    assert_eq!(hdmi::find(&mut direct, 0, &graph).unwrap(), None);
}

#[test]
fn binding_programs_every_converter_and_sends_the_infoframe_last() {
    let mut direct = Direct(intel([0, 0, MONITOR, 0]));
    let graph = read_graph(&mut direct, 0).unwrap();
    let found = hdmi::find(&mut direct, 0, &graph).unwrap().unwrap();
    direct.0.log.clear();
    hdmi::bind(&mut direct, 0, &found, 1, 0x11, 2).unwrap();
    let sets = log(&direct.0);
    for nid in [2u8, 4] {
        let of = |verb: u16| -> Vec<u8> {
            sets.iter()
                .filter(|&&(n, v, _)| n == nid && v == verb)
                .map(|&(_, _, p)| p)
                .collect()
        };
        assert_eq!(of(0x706), [0x10], "stream tag 1, channel 0");
        assert_eq!(of(0x70D), [0x01], "digital out on, other bits kept");
        assert_eq!(of(0x72D), [1], "two channels");
        assert_eq!(
            of(0x734),
            [0x00, 0x11, 0x2F, 0x3F, 0x4F, 0x5F, 0x6F, 0x7F],
            "slots 0 and 1 carry channels 0 and 1, the rest are unused"
        );
    }
    // The infoframe goes to the pin: stop, index 0, 14 bytes, start.
    let pin: Vec<_> = sets.iter().filter(|&&(n, _, _)| n == 6).collect();
    assert_eq!((pin[0].1, pin[0].2), (0x732, 0x00));
    assert_eq!((pin[1].1, pin[1].2), (0x730, 0x00));
    let data: Vec<u8> = pin[2..pin.len() - 1].iter().map(|&&(_, _, p)| p).collect();
    assert_eq!(data, hdmi::audio_infoframe(2));
    assert_eq!((pin[pin.len() - 1].1, pin[pin.len() - 1].2), (0x732, 0xC0));
    assert_eq!(
        sets.last().unwrap().1,
        0x732,
        "the infoframe is the last thing sent"
    );
}

#[test]
fn unbinding_sends_no_infoframe_and_keeps_the_digital_bits() {
    let mut codec = intel([0, 0, MONITOR, 0]);
    codec.nodes.get_mut(&2).unwrap().digital = 0x80_0003;
    let mut direct = Direct(codec);
    let graph = read_graph(&mut direct, 0).unwrap();
    let found = hdmi::find(&mut direct, 0, &graph).unwrap().unwrap();
    direct.0.log.clear();
    hdmi::bind(&mut direct, 0, &found, 0, 0, 2).unwrap();
    let sets = log(&direct.0);
    assert!(sets.iter().all(|&(n, _, _)| n != 6), "nothing for the pin");
    assert!(sets.contains(&(2, 0x706, 0x00)));
    assert!(
        sets.contains(&(2, 0x70D, 0x03)),
        "bit 1 of the low byte survives"
    );
}

#[test]
fn the_audio_infoframe_sums_to_zero_for_every_channel_count() {
    for channels in 0..=9u8 {
        let frame = hdmi::audio_infoframe(channels);
        assert_eq!(frame[..3], [0x84, 0x01, 0x0A]);
        assert_eq!(frame[4], channels.clamp(1, 8) - 1);
        assert_eq!(
            frame.iter().fold(0u8, |s, &b| s.wrapping_add(b)),
            0,
            "{channels}"
        );
    }
}

#[test]
fn enable_powers_and_unmutes_the_chosen_pin_only() {
    let mut direct = Direct(intel([0, 0, MONITOR, 0]));
    let graph = read_graph(&mut direct, 0).unwrap();
    let found = hdmi::find(&mut direct, 0, &graph).unwrap().unwrap();
    direct.0.log.clear();
    hdmi::enable(&mut direct, 0, &found).unwrap();
    let sets = log(&direct.0);
    assert!(sets.contains(&(6, 0x707, 0x40)));
    assert!(sets.iter().all(|&(n, v, _)| v != 0x707 || n == 6));
}

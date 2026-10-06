//! Host tests for the codec walk and path programming: QEMU's `hda-output`,
//! a board-style codec with mixers, selectors and unwired pins, and seeded
//! random graphs from a codec that lies.

use std::vec::Vec;

use crate::codec::{find_output, read_graph, CodecError, Hop, MAX_CONNS};
use crate::fake::{node, qemu_output, wcaps, Codec, Node};
use crate::program::Output;
use crate::verbs::{amp, caps::*, param, pin, VerbError, Verbs};
use crate::verbs::{
    SET_AMP_GAIN_MUTE, SET_CONN_SELECT, SET_EAPD, SET_PIN_CONTROL, SET_STREAM_CHANNEL,
};

/// The codec answering directly, without a controller in between.
struct Direct(Codec);

impl Verbs for Direct {
    fn send(&mut self, command: u32) -> Result<u32, VerbError> {
        Ok(self.0.answer(command))
    }
}

fn hop(nid: u8, input: Option<u8>) -> Hop {
    Hop { nid, input }
}

/// Every set-verb the codec saw, as `(nid, verb12, payload8)` for 12-bit verbs.
fn sets(codec: &Codec) -> Vec<(u8, u16, u8)> {
    codec
        .log
        .iter()
        .map(|&(nid, word)| (nid, (word >> 8) as u16, word as u8))
        .collect()
}

#[test]
fn qemu_output_path() {
    let mut direct = Direct(qemu_output());
    let graph = read_graph(&mut direct, 0).unwrap();
    assert_eq!(graph.afg, 1);
    let path = find_output(&graph).unwrap();
    assert_eq!(path.hops(), [hop(3, Some(0)), hop(2, None)]);
    let output = Output {
        codec: 0,
        graph: &graph,
        path,
    };
    output.enable(&mut direct).unwrap();
    output.bind(&mut direct, 1, 0x0011).unwrap();
    assert_eq!(output.channels(), 2);
    assert_eq!(output.pcm(&mut direct), Ok(1 << 17 | 0x1FC));
    let log = &direct.0.log;
    // The DAC's output amp at its 0 dB step (0x4A), both sides, unmuted.
    let unmute = (u32::from(SET_AMP_GAIN_MUTE) << 16)
        | u32::from(amp::OUTPUT | amp::LEFT | amp::RIGHT | 0x4A);
    assert!(log.contains(&(2, unmute)), "{log:x?}");
    let set = sets(&direct.0);
    assert!(set.contains(&(3, SET_PIN_CONTROL, pin::CTL_OUT_ENABLE)));
    assert!(set.contains(&(2, SET_STREAM_CHANNEL, 0x10)));
    // One input: no select, and no EAPD on a pin that has none.
    assert!(!set
        .iter()
        .any(|&(_, verb, _)| verb == SET_CONN_SELECT || verb == SET_EAPD));
}

/// A board-style codec: two DACs, a mixer, a selector, a speaker with EAPD, a
/// headphone jack, a microphone (input only) and a line out wired to nothing.
fn board() -> Codec {
    let mut codec = Codec::default();
    let afg_amp = (param::AMP_OUT_CAPS, 0x80 << 24 | 0x27 << 8 | 0x1F);
    codec
        .nodes
        .insert(0, node(&[(param::NODE_COUNT, 1 << 16 | 1)], &[], 0));
    codec.nodes.insert(
        1,
        node(
            &[
                (param::FUNCTION_TYPE, 1),
                (param::NODE_COUNT, 2 << 16 | 0x1A),
                afg_amp,
                (param::AMP_IN_CAPS, 0x80 << 24 | 0x17),
            ],
            &[],
            0,
        ),
    );
    let dac = node(
        &[(param::WIDGET_CAPS, wcaps(OUTPUT, STEREO | OUT_AMP))],
        &[],
        0,
    );
    codec.nodes.insert(0x02, dac.clone());
    codec.nodes.insert(0x03, dac);
    codec.nodes.insert(
        0x0C,
        node(
            &[
                (
                    param::WIDGET_CAPS,
                    wcaps(MIXER, STEREO | IN_AMP | CONN_LIST),
                ),
                (param::CONN_LIST_LEN, 2),
            ],
            &[0x02, 0x0B],
            0,
        ),
    );
    codec.nodes.insert(
        0x0D,
        node(
            &[
                (
                    param::WIDGET_CAPS,
                    wcaps(SELECTOR, STEREO | CONN_LIST | OUT_AMP),
                ),
                (param::CONN_LIST_LEN, 2),
            ],
            &[0x0C, 0x03],
            0,
        ),
    );
    let out_pin = |device: u32, connectivity: u32, conn: u16, extra: u32| {
        node(
            &[
                (param::WIDGET_CAPS, wcaps(PIN, STEREO | CONN_LIST)),
                (param::PIN_CAPS, pin::CAP_OUTPUT | extra),
                (param::CONN_LIST_LEN, 1),
            ],
            &[conn],
            connectivity << 30 | device << 20,
        )
    };
    codec.nodes.insert(0x14, out_pin(1, 2, 0x0D, pin::CAP_EAPD));
    codec
        .nodes
        .insert(0x15, out_pin(2, 0, 0x0C, pin::CAP_HEADPHONE));
    codec.nodes.insert(0x1B, out_pin(0, 1, 0x02, 0));
    codec.nodes.insert(
        0x18,
        node(
            &[
                (param::WIDGET_CAPS, wcaps(PIN, STEREO)),
                (param::PIN_CAPS, 1 << 5),
            ],
            &[],
            0xA << 20,
        ),
    );
    codec
}

#[test]
fn board_codec_prefers_an_attached_speaker_and_routes_through_the_selector() {
    let mut direct = Direct(board());
    let graph = read_graph(&mut direct, 0).unwrap();
    let path = find_output(&graph).unwrap();
    // The line out at 0x1B is wired to nothing; the speaker at 0x14 wins, and
    // its selector's second input is a DAC directly.
    assert_eq!(
        path.hops(),
        [hop(0x14, Some(0)), hop(0x0D, Some(1)), hop(0x03, None)]
    );
    let output = Output {
        codec: 0,
        graph: &graph,
        path,
    };
    output.enable(&mut direct).unwrap();
    let set = sets(&direct.0);
    assert!(set.contains(&(0x0D, SET_CONN_SELECT, 1)));
    assert!(set.contains(&(0x14, SET_EAPD, pin::EAPD_ON)));
    assert!(set.contains(&(0x14, SET_PIN_CONTROL, pin::CTL_OUT_ENABLE)));
    // Amps without their own capabilities use the function group's 0 dB step.
    let selector_amp = (u32::from(SET_AMP_GAIN_MUTE) << 16)
        | u32::from(amp::OUTPUT | amp::LEFT | amp::RIGHT | 0x1F);
    assert!(direct.0.log.contains(&(0x0D, selector_amp)));
}

#[test]
fn a_headphone_only_board_enables_the_headphone_amp_and_the_mixer_input() {
    let mut codec = board();
    codec.nodes.remove(&0x14);
    let mut direct = Direct(codec);
    let graph = read_graph(&mut direct, 0).unwrap();
    let path = find_output(&graph).unwrap();
    assert_eq!(
        path.hops(),
        [hop(0x15, Some(0)), hop(0x0C, Some(0)), hop(0x02, None)]
    );
    let output = Output {
        codec: 0,
        graph: &graph,
        path,
    };
    output.enable(&mut direct).unwrap();
    let set = sets(&direct.0);
    assert!(set.contains(&(
        0x15,
        SET_PIN_CONTROL,
        pin::CTL_OUT_ENABLE | pin::CTL_HP_ENABLE
    )));
    let mixer_in = (u32::from(SET_AMP_GAIN_MUTE) << 16)
        | u32::from(amp::INPUT | amp::LEFT | amp::RIGHT | 0x17);
    assert!(
        direct.0.log.contains(&(0x0C, mixer_in)),
        "{:x?}",
        direct.0.log
    );
}

#[test]
fn connection_ranges_and_long_lists_are_expanded_and_bounded() {
    let mut codec = qemu_output();
    // Short form: 2, then a range up to 9 (bit 7), then 3: capped at MAX_CONNS.
    let mut pin = codec.nodes[&3].clone();
    pin.conn_raw = std::vec![0x02, 0x89, 0x03];
    pin.params.insert(param::CONN_LIST_LEN, 3);
    codec.nodes.insert(3, pin.clone());
    let mut direct = Direct(codec.clone());
    let graph = read_graph(&mut direct, 0).unwrap();
    let conns: Vec<u8> = graph.widget(3).unwrap().connections().to_vec();
    assert_eq!(conns, [2, 3, 4, 5, 6, 7, 8, 9, 3]);
    // Long form with a huge range: bounded.
    pin.long_form = true;
    pin.conn_raw = std::vec![0x0002, 0xFFFF];
    pin.params.insert(param::CONN_LIST_LEN, 0x80 | 2);
    codec.nodes.insert(3, pin);
    let mut direct = Direct(codec);
    let graph = read_graph(&mut direct, 0).unwrap();
    assert_eq!(graph.widget(3).unwrap().connections().len(), MAX_CONNS);
    assert!(
        find_output(&graph).is_ok(),
        "the DAC is still the first connection"
    );
}

#[test]
fn no_audio_function_or_no_path_is_an_error() {
    let mut codec = qemu_output();
    codec
        .nodes
        .get_mut(&1)
        .unwrap()
        .params
        .insert(param::FUNCTION_TYPE, 2);
    assert!(matches!(
        read_graph(&mut Direct(codec), 0),
        Err(CodecError::NoAudioFunction)
    ));
    let mut codec = qemu_output();
    codec
        .nodes
        .get_mut(&3)
        .unwrap()
        .params
        .insert(param::PIN_CAPS, 0);
    let graph = read_graph(&mut Direct(codec), 0).unwrap();
    assert_eq!(find_output(&graph), Err(CodecError::NoPath));
}

/// Seeded: random graphs from a codec that lies about counts, types and
/// connections (cycles included). The walk must never panic, and any path it
/// returns must be real: an output-capable pin first, a converter last, and
/// each hop's input index naming the next hop.
#[test]
fn seeded_random_graphs() {
    fuzzkit::for_seeds("hda::seeded_random_graphs", |_, rng| {
        let mut codec = Codec::default();
        let count = rng.range(1, 40) as u32;
        codec
            .nodes
            .insert(0, node(&[(param::NODE_COUNT, 1 << 16 | 1)], &[], 0));
        let afg_count = if rng.one_in(8) { 0xFF } else { count };
        codec.nodes.insert(
            1,
            node(
                &[
                    (param::FUNCTION_TYPE, 1),
                    (param::NODE_COUNT, 2 << 16 | afg_count),
                ],
                &[],
                0,
            ),
        );
        for nid in 2..2 + count as u8 {
            let kind = rng.below(8) as u32;
            let mut n: Node = node(
                &[(param::WIDGET_CAPS, wcaps(kind, rng.next_u32() & 0xFFF))],
                &[],
                rng.next_u32(),
            );
            let conns = rng.below(6) as usize;
            n.conn_raw = (0..conns)
                .map(|_| rng.below(48) as u16 | if rng.one_in(6) { 0x80 } else { 0 })
                .collect();
            n.params.insert(
                param::CONN_LIST_LEN,
                conns as u32 | if rng.one_in(10) { 0x7F } else { 0 },
            );
            n.params.insert(param::PIN_CAPS, rng.next_u32());
            codec.nodes.insert(nid, n);
        }
        let mut direct = Direct(codec);
        let Ok(graph) = read_graph(&mut direct, 0) else {
            return;
        };
        let Ok(path) = find_output(&graph) else {
            return;
        };
        let hops = path.hops();
        let first = graph.widget(hops[0].nid).unwrap();
        assert_eq!(first.kind(), PIN);
        assert_ne!(first.pin_caps & pin::CAP_OUTPUT, 0);
        assert_eq!(graph.widget(path.dac()).unwrap().kind(), OUTPUT);
        for pair in hops.windows(2) {
            let widget = graph.widget(pair[0].nid).unwrap();
            let index = usize::from(pair[0].input.unwrap());
            assert_eq!(widget.connections()[index], pair[1].nid);
        }
        let output = Output {
            codec: 0,
            graph: &graph,
            path,
        };
        output.enable(&mut direct).unwrap();
    });
}

//! The controls a UI drives while a song plays: seeking, muting, live
//! separation and interpolation, levels, the song's names, and owning the
//! module.

use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use crate::synth::{effect, note, square, ModBuilder};
use crate::{Module, Options, Player, Position};

const RATE: u32 = 48_000;
/// Frames in one row at speed 6, tempo 125, 48 kHz.
const ROW: usize = 6 * 960;

fn tone() -> ModBuilder {
    ModBuilder::new().sample(1, square(32), 64, true)
}

/// Two patterns; channel 0 plays in both, channel 1 in the second only.
fn two_patterns() -> Module {
    let bytes = tone()
        .orders(&[0, 1])
        .note(0, 0, 0, note(214, 1))
        .note(1, 0, 0, note(428, 1))
        .note(1, 0, 1, note(320, 1))
        .build();
    Module::parse(&bytes).unwrap()
}

fn render<M: core::borrow::Borrow<Module>>(player: &mut Player<M>, frames: usize) -> Vec<i16> {
    let mut out = vec![0i16; frames * 2];
    let n = player.render(&mut out);
    out.truncate(n * 2);
    out
}

fn options() -> Options {
    Options {
        separation: 100,
        interpolate: false,
        ..Options::default()
    }
}

#[test]
fn names_and_title_are_parsed() {
    let bytes = tone()
        .title("A demo song for test")
        .name(1, "square lead")
        .name(31, "hello from slot 31")
        .build();
    let module = Module::parse(&bytes).unwrap();
    assert_eq!(&module.title, b"A demo song for test");
    assert_eq!(&module.samples[0].name[..11], b"square lead");
    assert!(module.samples[0].name[11..].iter().all(|&b| b == 0));
    assert_eq!(&module.samples[30].name[..18], b"hello from slot 31");
}

#[test]
fn an_owning_player_renders_like_a_borrowing_one() {
    let module = two_patterns();
    let mut borrowed = Player::new(&module, RATE, options());
    let mut owned = Player::new(Rc::new(module.clone()), RATE, options());
    assert_eq!(render(&mut borrowed, 3 * ROW), render(&mut owned, 3 * ROW));
    assert_eq!(owned.module().orders, module.orders);
    assert_eq!(owned.rate(), RATE);
}

#[test]
fn seek_moves_to_the_first_row_of_an_order() {
    let module = two_patterns();
    let mut player = Player::new(&module, RATE, options());
    render(&mut player, ROW / 2);
    player.seek(1);
    assert_eq!(player.position(), Position { order: 1, row: 0 });
    // The first frames after the seek are exactly the second pattern's start.
    let mut fresh = Player::new(&module, RATE, options());
    fresh.seek(1);
    assert_eq!(render(&mut player, ROW), render(&mut fresh, ROW));
    // Out of range clamps to the last order.
    player.seek(99);
    assert_eq!(player.position().order, 1);
}

#[test]
fn seek_restarts_a_finished_song() {
    let module = two_patterns();
    let mut player = Player::new(&module, RATE, options());
    while !render(&mut player, 8192).is_empty() {}
    assert!(player.is_finished());
    player.seek(0);
    assert!(!player.is_finished());
    assert_eq!(render(&mut player, ROW).len(), 2 * ROW);
}

#[test]
fn a_muted_channel_is_silent_and_rejoins_in_time() {
    let module = two_patterns();
    let mut player = Player::new(&module, RATE, options());
    player.set_muted(0, true);
    assert!(player.is_muted(0));
    assert!(render(&mut player, ROW).iter().all(|&s| s == 0));
    assert_eq!(player.levels()[0], 0, "a muted channel reads 0");
    player.set_muted(0, false);
    // Unmuted, it sounds exactly as if it had never been muted.
    let mut reference = Player::new(&module, RATE, options());
    render(&mut reference, ROW);
    assert_eq!(render(&mut player, ROW), render(&mut reference, ROW));
    assert!(!player.is_muted(7), "no such channel");
    player.set_muted(7, true); // ignored
}

#[test]
fn muting_an_idle_channel_changes_nothing() {
    let module = two_patterns();
    let mut a = Player::new(&module, RATE, options());
    let mut b = Player::new(&module, RATE, options());
    b.set_muted(3, true);
    assert_eq!(render(&mut a, 2 * ROW), render(&mut b, 2 * ROW));
}

#[test]
fn separation_and_interpolation_change_live() {
    let module = two_patterns();
    let mono = Options {
        separation: 0,
        ..options()
    };
    let mut live = Player::new(&module, RATE, options());
    let mut reference = Player::new(&module, RATE, mono);
    live.set_separation(0);
    let out = render(&mut live, ROW);
    assert_eq!(out, render(&mut reference, ROW));
    assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]), "mono");
    live.set_interpolate(true);
    reference.set_interpolate(true);
    assert_eq!(render(&mut live, ROW), render(&mut reference, ROW));
}

#[test]
fn levels_follow_sounding_voices() {
    // A one-shot sample shorter than a row: loud at first, then silent.
    let bytes = ModBuilder::new()
        .sample(1, square(64), 48, false)
        .note(0, 0, 2, note(428, 1))
        .build();
    let module = Module::parse(&bytes).unwrap();
    let mut player = Player::new(&module, RATE, options());
    render(&mut player, 16);
    assert_eq!(player.levels(), [0, 0, 48, 0]);
    render(&mut player, ROW);
    assert_eq!(player.levels(), [0, 0, 0, 0], "the sample ran out");
}

#[test]
fn speed_and_tempo_are_reported() {
    let bytes = tone()
        .note(0, 0, 0, effect(0xF, 3))
        .note(0, 0, 1, effect(0xF, 150))
        .build();
    let module = Module::parse(&bytes).unwrap();
    let mut player = Player::new(&module, RATE, options());
    assert_eq!((player.speed(), player.tempo()), (6, 125));
    render(&mut player, 1);
    assert_eq!((player.speed(), player.tempo()), (3, 150));
}

#[test]
fn a_loop_range_sustains_after_the_attack() {
    let mut data = square(64);
    data.extend(square(16));
    let bytes = ModBuilder::new()
        .sample(1, data, 64, false)
        .loop_range(1, 64, 16)
        .note(0, 0, 0, note(428, 1))
        .build();
    let module = Module::parse(&bytes).unwrap();
    assert_eq!(module.samples[0].loop_range, Some((64, 80)));
    let mut player = Player::new(&module, RATE, options());
    let out = render(&mut player, 4 * ROW);
    assert!(
        out[out.len() - 200..].iter().any(|&s| s != 0),
        "still sounding"
    );
}

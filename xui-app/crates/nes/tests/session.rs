//! The session against the sample ROM (`tests/data/sample.nes`): the console
//! runs the app's own cartridge, the picture moves, the APU sounds the melody
//! the ROM plays, the controller reaches the game, and the save files
//! round-trip. Everything here runs on the host; the QEMU session only has to
//! show the same app in a window.

use std::path::{Path, PathBuf};

use tetanes_core::input::JoypadBtn;
use xui_nes::session::{Error, Session};

const SAMPLE: &[u8] = include_bytes!("data/sample.nes");

/// A fresh directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xui-nes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a test directory");
    dir
}

/// The sample ROM written under `dir`.
fn rom_file(dir: &Path) -> PathBuf {
    let path = dir.join("sample.nes");
    std::fs::write(&path, SAMPLE).expect("the sample ROM");
    path
}

fn console(dir: &Path) -> Session {
    Session::new(dir.to_path_buf(), 48_000.0)
}

fn run(session: &mut Session, frames: usize) {
    for _ in 0..frames {
        session.clock().expect("the console runs");
    }
}

/// A cheap hash of one frame, to compare pictures.
fn hash(frame: &[u8]) -> u64 {
    frame.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

#[test]
fn the_sample_rom_runs_and_draws() {
    let dir = temp_dir("runs");
    let mut session = console(&dir);
    let info = session.load_rom(&rom_file(&dir)).expect("the sample loads");
    assert_eq!(
        (info.mapper, info.prg_kb, info.chr_kb, info.battery),
        (0, 32, 8, true)
    );
    run(&mut session, 60);
    // The ROM counts its own frames into work RAM ($12/$13).
    let counted = u16::from(session.wram()[0x12]) | (u16::from(session.wram()[0x13]) << 8);
    assert!(counted >= 55, "the ROM counted {counted} frames");
    // The picture has the title text and the ball on a blue backdrop.
    let frame = session.frame().to_vec();
    let mut colors = std::collections::HashSet::new();
    let mut white = false;
    for pixel in frame.chunks_exact(4) {
        colors.insert((pixel[0], pixel[1], pixel[2]));
        white |= pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200;
    }
    assert!(colors.len() >= 3, "only {} colours", colors.len());
    assert!(white, "the title text is drawn");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_sample_rom_sounds_the_melody() {
    let dir = temp_dir("sound");
    let mut session = console(&dir);
    session.load_rom(&rom_file(&dir)).expect("the sample loads");
    // Let the ROM start, then collect the first note (30 frames of A4).
    run(&mut session, 5);
    let mut samples = Vec::new();
    for _ in 0..25 {
        session.clock().expect("the console runs");
        samples.extend_from_slice(session.samples());
    }
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    // The APU mixes to the NES's own non-linear scale, so a full-volume pulse
    // peaks near 0.06; anything near zero means the channel never opened.
    assert!(peak > 0.02, "the APU is nearly silent (peak {peak})");
    let hz = dominant_hz(&samples, 48_000.0);
    assert!(
        (hz - 440.0).abs() < 66.0,
        "the first note sounds at {hz:.1} Hz, not A4"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The frequency with the strongest autocorrelation in `samples`, between
/// 100 Hz and 2 kHz. Enough to say which note a square wave is.
fn dominant_hz(samples: &[f32], rate: f32) -> f32 {
    let samples = &samples[samples.len() / 4..samples.len() * 3 / 4];
    let mut best = (0.0f32, 100.0f32);
    for hz in (100..=2000).step_by(2) {
        let lag = (rate / hz as f32) as usize;
        if lag == 0 || lag >= samples.len() {
            continue;
        }
        let sum: f32 = samples
            .iter()
            .zip(&samples[lag..])
            .map(|(a, b)| a * b)
            .sum();
        if sum > best.0 {
            best = (sum, hz as f32);
        }
    }
    best.1
}

#[test]
fn the_controller_reaches_the_rom() {
    let dir = temp_dir("input");
    let mut session = console(&dir);
    session.load_rom(&rom_file(&dir)).expect("the sample loads");
    run(&mut session, 10);
    assert_eq!(session.wram()[0x10], 0, "nothing is held");
    let still = hash(session.frame());

    // Right is bit 0 of the byte the ROM reads back.
    session.press(JoypadBtn::Right, true);
    run(&mut session, 4);
    assert_eq!(session.wram()[0x10] & 0x01, 0x01);
    run(&mut session, 8);
    let moved = hash(session.frame());
    assert_ne!(still, moved, "the ball moved");
    session.press(JoypadBtn::Right, false);
    run(&mut session, 2);
    assert_eq!(session.wram()[0x10] & 0x01, 0x00);

    // A is bit 7; the ROM beeps on the edge (the sound test hears the note).
    session.press(JoypadBtn::A, true);
    run(&mut session, 2);
    assert_eq!(session.wram()[0x10] & 0x80, 0x80);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_save_state_restores_the_console() {
    let dir = temp_dir("state");
    let rom = rom_file(&dir);
    let mut saved = console(&dir);
    saved.load_rom(&rom).expect("the sample loads");
    run(&mut saved, 40);
    let path = saved.save_state(1).expect("the state saves");
    assert!(path.exists() && saved.has_state(1) && !saved.has_state(2));
    run(&mut saved, 10);
    saved.load_state(1).expect("the state loads");
    run(&mut saved, 5);
    let restored = hash(saved.frame());
    let wram = saved.wram().to_vec();

    // The same five frames from the same point, without the state.
    let mut plain = console(&dir);
    plain.load_rom(&rom).expect("the sample loads");
    run(&mut plain, 40);
    run(&mut plain, 5);
    assert_eq!(restored, hash(plain.frame()), "the pictures agree");
    assert_eq!(wram, plain.wram().to_vec(), "work RAM agrees");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_battery_save_round_trips() {
    let dir = temp_dir("battery");
    let rom = rom_file(&dir);
    let mut session = console(&dir);
    session.load_rom(&rom).expect("the sample loads");
    run(&mut session, 30);
    let sram = session.sram().to_vec();
    assert!(sram.iter().any(|byte| *byte != 0), "the ROM wrote PRG RAM");
    let path = session
        .save_battery()
        .expect("saved")
        .expect("a battery path");
    assert!(path.exists());
    drop(session);

    let mut again = console(&dir);
    again.load_rom(&rom).expect("the sample loads again");
    assert_eq!(again.sram(), &sram[..], "the battery came back");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_file_is_refused_and_nothing_loads() {
    let dir = temp_dir("bad");
    let path = dir.join("junk.nes");
    std::fs::write(&path, b"not a cartridge at all").unwrap();
    let mut session = console(&dir);
    assert!(matches!(session.load_rom(&path), Err(Error::BadRom(_))));
    assert!(!session.is_loaded());

    // A header with no PRG is refused too.
    let mut header = vec![0u8; 16];
    header[..4].copy_from_slice(b"NES\x1a");
    std::fs::write(&path, &header).unwrap();
    assert!(matches!(session.load_rom(&path), Err(Error::BadRom(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_power_cycle_starts_the_rom_over() {
    let dir = temp_dir("cycle");
    let mut session = console(&dir);
    session.load_rom(&rom_file(&dir)).expect("the sample loads");
    run(&mut session, 40);
    assert!(session.frames() >= 40);
    session.power_cycle().expect("the power cycle reloads");
    assert_eq!(session.frames(), 0);
    run(&mut session, 2);
    let counted = u16::from(session.wram()[0x12]) | (u16::from(session.wram()[0x13]) << 8);
    assert!(counted < 5, "the ROM started counting over ({counted})");
    let _ = std::fs::remove_dir_all(&dir);
}

//! The whole path on a real Motion JPEG file: open, decode, pace, pause,
//! seek, finish; and the ways a file can be wrong.

use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lazyvideo::avi::build_avi;
use lazyvideo::{open, open_reader, Error, Player, Status};

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample.avi")
}

/// Polls until `done` holds for the status, taking frames as a window would.
fn wait_for(player: &Player, what: &str, done: impl Fn(&Status) -> bool) -> Status {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = player.take_frame() {
            player.recycle(frame.rgba);
        }
        let status = player.status();
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_sample_reports_what_the_file_holds() {
    let media = open(&sample()).unwrap();
    let info = &media.info;
    assert_eq!((info.width, info.height), (160, 120));
    assert_eq!((info.fps_num, info.fps_den), (25, 1));
    assert_eq!(info.frames, 50);
    assert_eq!(info.duration_us(), 2_000_000);
    assert!(matches!(info.codec_name().as_str(), "MJPG" | "mjpg"));
}

/// The clip every desktop image ships (`tools/video/make_sample.py`): it must
/// open as the player's session expects.
#[test]
fn the_shipped_sample_is_what_the_session_expects() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../assets/samples/lazyos-sample.avi");
    let info = open(&path).unwrap().info;
    assert_eq!((info.width, info.height), (320, 180));
    assert_eq!((info.fps_num, info.fps_den, info.frames), (25, 1, 125));
    assert_eq!(info.codec_name(), "MJPG");
}

#[test]
fn it_plays_to_the_end_in_about_the_time_the_video_lasts() {
    let started = Instant::now();
    let player = Player::start(open(&sample()).unwrap());
    let status = wait_for(&player, "the end", |s| s.ended);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1_800),
        "ran ahead of the clock: {took:?}"
    );
    assert!(took < Duration::from_millis(4_000), "ran slow: {took:?}");
    assert_eq!(status.position_us, 2_000_000);
    assert!(!status.playing && status.error.is_none());
    // Every frame was either shown or counted as dropped.
    assert!(status.shown + status.dropped >= 49, "{status:?}");
    assert!(status.shown >= 25, "{status:?}");
}

#[test]
fn a_taken_frame_is_a_full_rgba_picture() {
    let player = Player::start(open(&sample()).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = player.take_frame() {
            break frame;
        }
        assert!(Instant::now() < deadline, "no frame arrived");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!((frame.width, frame.height), (160, 120));
    assert_eq!(frame.rgba.len(), 160 * 120 * 4);
    assert!(frame.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    // testsrc2 is not one flat colour.
    let first = &frame.rgba[..3];
    assert!(frame
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| &p[..3] != first));
}

#[test]
fn pause_holds_the_picture_and_play_resumes() {
    let player = Player::start(open(&sample()).unwrap());
    wait_for(&player, "the first third", |s| s.position_us > 500_000);
    player.pause();
    let held = wait_for(&player, "the pause", |s| !s.playing);
    std::thread::sleep(Duration::from_millis(300));
    let later = player.status();
    assert!(
        later.position_us.abs_diff(held.position_us) < 50_000,
        "moved while paused: {held:?} -> {later:?}"
    );
    player.play();
    wait_for(&player, "playing again", |s| {
        s.position_us > held.position_us + 200_000
    });
}

#[test]
fn seek_shows_the_picture_there_even_while_paused() {
    let player = Player::start(open(&sample()).unwrap());
    wait_for(&player, "a start", |s| s.shown > 0);
    player.pause();
    wait_for(&player, "the pause", |s| !s.playing);
    while player.take_frame().is_some() {}
    player.seek(1_500_000);
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = player.take_frame() {
            break frame;
        }
        assert!(Instant::now() < deadline, "no picture after the seek");
        std::thread::sleep(Duration::from_millis(2));
    };
    // The frame showing at 1.5 s is the one that began at 1.48 s.
    assert_eq!(frame.pts_us, 1_480_000);
    assert!(!player.status().playing, "a seek must not start playback");
}

#[test]
fn play_after_the_end_starts_over() {
    let player = Player::start(open(&sample()).unwrap());
    wait_for(&player, "the end", |s| s.ended);
    player.play();
    let status = wait_for(&player, "the replay", |s| !s.ended && s.playing);
    assert!(status.position_us < 1_000_000);
    wait_for(&player, "the second end", |s| s.ended);
}

#[test]
fn dropping_the_player_stops_the_thread_promptly() {
    let player = Player::start(open(&sample()).unwrap());
    let started = Instant::now();
    drop(player);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn a_codec_without_a_decoder_is_named_in_the_error() {
    let bytes = build_avi(b"H264", 64, 48, 25, &[vec![0; 10]]);
    match open_reader(Cursor::new(bytes)) {
        Err(Error::Unsupported(codec)) => assert_eq!(codec, "H264"),
        other => panic!("expected Unsupported, got {:?}", other.err()),
    }
}

#[test]
fn undecodable_frames_stop_playback_with_a_reason() {
    let frames: Vec<Vec<u8>> = (0..90).map(|i| vec![i as u8; 64]).collect();
    let bytes = build_avi(b"MJPG", 64, 48, 200, &frames);
    let player = Player::start(open_reader(Cursor::new(bytes)).unwrap());
    let status = wait_for(&player, "the failure", |s| s.error.is_some());
    assert!(!status.playing);
    assert_eq!(status.shown, 0);
}

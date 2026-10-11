//! `cargo run --release -p lazyvideo --example decode_bench -- clip.avi`
//!
//! Decodes every frame of a file back to back (no pacing) and prints the
//! cost per frame, the number that decides what a file can play at.

use std::time::Instant;

use lazyvideo::{open, Demuxer};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: decode_bench <file.avi>");
        std::process::exit(2);
    };
    let media = match open(path.as_ref()) {
        Ok(media) => media,
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    };
    let info = media.info.clone();
    let (mut demux, mut decoder): (Box<dyn Demuxer>, _) = media.into_parts();
    let (mut packet, mut rgba) = (Vec::new(), Vec::new());
    let started = Instant::now();
    let mut decoded = 0u32;
    for index in 0..demux.frames() {
        if demux.read(index, &mut packet).is_err() {
            break;
        }
        if decoder.decode(&packet, &mut rgba).is_ok() {
            decoded += 1;
        }
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0 / decoded.max(1) as f64;
    println!(
        "{}x{} {} {:.1} fps: {decoded} frames, {ms:.2} ms/frame ({:.0} fps decode speed, {:.0}% of one core at the file's rate)",
        info.width,
        info.height,
        info.codec_name(),
        info.fps(),
        1000.0 / ms,
        ms * info.fps() / 10.0
    );
}

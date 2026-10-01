//! Render a module to a 16-bit stereo WAV on the host:
//! `cargo run -p modplay --example render_wav -- song.mod out.wav [rate]`.
//! Useful for listening to the player without booting the OS.

use std::io::Write;

use modplay::{Module, Options, Player};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(output)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: render_wav <in.mod> <out.wav> [rate]");
        std::process::exit(2);
    };
    let rate: u32 = args.get(3).and_then(|r| r.parse().ok()).unwrap_or(48_000);
    let module = Module::parse(&std::fs::read(input)?).unwrap_or_else(|e| {
        eprintln!("{input}: {e:?}");
        std::process::exit(1);
    });
    let mut player = Player::new(&module, rate, Options::default());
    let mut pcm = Vec::new();
    let mut buf = [0i16; 4096];
    loop {
        let frames = player.render(&mut buf);
        if frames == 0 {
            break;
        }
        for s in &buf[..frames * 2] {
            pcm.extend_from_slice(&s.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(output)?;
    let len = pcm.len() as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&[1, 0, 2, 0])?; // PCM, stereo
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&[4, 0, 16, 0])?;
    f.write_all(b"data")?;
    f.write_all(&len.to_le_bytes())?;
    f.write_all(&pcm)
}

//! A development probe: run the sample ROM headlessly and print what the
//! console produced. `cargo run -p xui-nes --example probe -- [frames]`

use xui_nes::Session;
fn main() {
    let frames: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(120);
    let dir = std::env::temp_dir().join("xui-nes-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory");
    let rom = dir.join("sample.nes");
    std::fs::write(&rom, include_bytes!("../tests/data/sample.nes")).expect("the sample");
    let mut session = Session::new(dir.clone(), 48_000.0);
    let info = session.load_rom(&rom).expect("loads");
    println!("rom: {}", info.summary());
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for frame in 0..frames {
        session.clock().expect("runs");
        for sample in session.samples() {
            peak = peak.max(sample.abs());
            let sample = f64::from(*sample);
            sum += sample * sample;
            count += 1;
        }
        if frame % 30 == 29 {
            let wram = session.wram();
            println!(
                "frame {:3}: joy=${:02X} frames=${:04X} note={} peak={peak:.4} rms={:.4} samples={count}",
                frame + 1,
                wram[0x10],
                u16::from(wram[0x12]) | (u16::from(wram[0x13]) << 8),
                wram[0x21],
                (sum / count as f64).sqrt(),
            );
        }
    }
    let frame = session.frame().to_vec();
    let mut histogram: std::collections::HashMap<(u8, u8, u8), usize> = Default::default();
    for pixel in frame.as_chunks::<4>().0 {
        *histogram.entry((pixel[0], pixel[1], pixel[2])).or_default() += 1;
    }
    let mut colors: Vec<_> = histogram.into_iter().collect();
    colors.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("colours: {} distinct", colors.len());
    for (color, count) in colors.iter().take(12) {
        println!("#{:02X}{:02X}{:02X} x{count}", color.0, color.1, color.2);
    }
    // The picture as a PPM, for looking at it (`PIL.Image.open`).
    let mut ppm = "P6\n256 240\n255\n".to_string().into_bytes();
    for pixel in frame.as_chunks::<4>().0 {
        ppm.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(dir.join("frame.ppm"), ppm).expect("a PPM");
    println!("wrote {}", dir.join("frame.ppm").display());
}

//! Host tool for the P0 spike and golden images: renders every page of the
//! given PDFs to PNG and prints per-page timings.
//!
//! `cargo run --release -p lazypdf --example render -- <out-dir> <dpi> <file.pdf>...`
//! (a `file.pdf:password` argument opens an encrypted file).

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [out, dpi, files @ ..] = args.as_slice() else {
        eprintln!("usage: render <out-dir> <dpi> <file.pdf[:password]>...");
        std::process::exit(2);
    };
    let scale: f32 = dpi.parse::<f32>().expect("dpi") / 72.0;
    std::fs::create_dir_all(out).expect("out dir");
    for arg in files {
        let (path, password) = arg
            .rsplit_once(".pdf:")
            .map_or((arg.as_str(), ""), |(p, pw)| (p, pw));
        let path = if path.ends_with(".pdf") {
            path.to_string()
        } else {
            format!("{path}.pdf")
        };
        let stem = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let data = std::fs::read(&path).expect("read");
        let t = Instant::now();
        let doc = match lazypdf::Document::open(data, password) {
            Ok(d) => d,
            Err(e) => {
                println!("{stem}: OPEN FAIL {}", e.reason());
                continue;
            }
        };
        let open_ms = t.elapsed().as_secs_f64() * 1e3;
        let r = lazypdf::Renderer::new(&doc);
        let mut times = Vec::new();
        let limit: usize = std::env::var("MAX_PAGES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);
        for i in 0..doc.page_count().min(limit) {
            let t = Instant::now();
            let tile = r.render_page(i, scale).expect("page");
            times.push(t.elapsed().as_secs_f64() * 1e3);
            let f = std::fs::File::create(format!("{out}/{stem}-{:04}.png", i + 1)).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), tile.width, tile.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&tile.rgba)
                .unwrap();
        }
        let total: f64 = times.iter().sum();
        let max = times.iter().cloned().fold(0.0, f64::max);
        println!(
            "{stem}: v{} pages={} open={open_ms:.1}ms render total={total:.0}ms avg={:.1}ms max={max:.1}ms title={:?}",
            doc.version(), doc.page_count(), total / times.len().max(1) as f64, doc.info().title
        );
    }
}

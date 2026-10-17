//! Converts a filter list file to WebKit rule lists and prints what it kept and dropped.
//!
//! ```text
//! cargo run --release -p lazyadblock --example convert -- easylist.txt out/easylist
//! ```
//!
//! writes `out/easylist-0.json`, `out/easylist-1.json`, ... (one per list part) and a
//! `Stats` line. The WebKit side of the check is `python tools/webkit/blockcheck.py`.

use std::time::Instant;

use lazyadblock::{convert, Options};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(prefix)) = (args.next(), args.next()) else {
        eprintln!("usage: convert <list.txt> <output-prefix> [max-rules-per-list]");
        std::process::exit(2);
    };
    let mut options = Options::default();
    if let Some(max) = args.next().and_then(|n| n.parse().ok()) {
        options.max_rules_per_list = max;
    }
    let bytes = std::fs::read(&input).unwrap_or_else(|e| {
        eprintln!("cannot read {input}: {e}");
        std::process::exit(1);
    });
    // As the browser reads a downloaded file: lossy, never an error.
    let text = String::from_utf8_lossy(&bytes);
    let started = Instant::now();
    let out = convert(&text, &options);
    let ms = started.elapsed().as_millis();
    for (n, list) in out.lists.iter().enumerate() {
        let path = format!("{prefix}-{n}.json");
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, &list.json).unwrap_or_else(|e| {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(1);
        });
        println!("{path}: {} rules, {} bytes", list.rules, list.json.len());
    }
    println!("{} ({} ms)", out.stats, ms);
}

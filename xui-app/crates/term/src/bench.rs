//! Throughput baseline: `cargo test -p lazyterm --release bench -- --ignored
//! --nocapture`. Mixed output through an 80x24 screen in 4 KiB chunks, the way
//! the Terminal drains the pty.

use std::time::Instant;

use crate::Term;

/// About `bytes` of plain lines, long (wrapping) lines and SGR-coloured lines.
fn mixed(bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes + 512);
    let mut n = 0usize;
    while out.len() < bytes {
        match n % 3 {
            0 => out.extend_from_slice(
                format!("{n:6} the quick brown fox jumps over the lazy dog\r\n").as_bytes(),
            ),
            1 => {
                out.extend_from_slice(b"long ");
                for _ in 0..5 {
                    out.extend_from_slice(b"0123456789abcdefghijklmnopqrstuvwxyz ");
                }
                out.extend_from_slice(b"\r\n");
            }
            _ => out.extend_from_slice(
                format!(
                    "\x1b[1;3{}mred\x1b[0m \x1b[38;5;{}mindexed\x1b[0m \x1b[38;2;10;{};200mtruecolor\x1b[0m end\r\n",
                    1 + n % 7,
                    n % 256,
                    n % 256
                )
                .as_bytes(),
            ),
        }
        n += 1;
    }
    out
}

#[test]
#[ignore]
fn bench_throughput() {
    let data = mixed(4 << 20);
    let mut best = 0f64;
    for round in 0..5 {
        let mut term = Term::new(80, 24);
        let start = Instant::now();
        for chunk in data.chunks(4096) {
            let _ = term.feed(chunk);
        }
        let rate = data.len() as f64 / (1 << 20) as f64 / start.elapsed().as_secs_f64();
        println!("lazyterm bench round {round}: {rate:.1} MiB/s");
        best = best.max(rate);
    }
    println!("lazyterm throughput best: {best:.1} MiB/s over {} bytes", data.len());
}

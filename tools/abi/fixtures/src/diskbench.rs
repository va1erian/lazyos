//! `diskbench` — storage throughput and exec latency on the OS volume
//! (docs/performance-plan.md P5), run by `tools/perf/disk.py` as the image's
//! `abi-init`.
//!
//! Every phase checks what it reads, so a faster path that returns wrong
//! bytes fails the bench instead of improving it. Prints one line per phase:
//!
//! ```text
//! DISK:<metric>:<value> <unit> [detail]
//! ```
//!
//! and finally `ABI:diskbench:PASS` (or `:FAIL:<why>`). The file is larger
//! than the kernel's block cache (1/32 of RAM, at most 32 MiB), so the read
//! passes go to the device for most of their blocks.

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{fail, pass};

const NAME: &str = "diskbench";
const DIR: &str = "/bench";
const BIG: &str = "/bench/big";
const MIB: usize = 1024 * 1024;
/// Size of the sequential file, in MiB: twice the largest block cache.
const BIG_MIB: usize = 64;
/// Bytes per `read`/`write` call of the sequential phases.
const IO: usize = MIB;
/// The program the exec phase runs, and how often.
const BUSYBOX: &str = "/system/bin/busybox";
const EXECS: usize = 20;
/// The small-file phase: what a package install writes.
const SMALL_FILES: usize = 400;
const SMALL_BYTES: usize = 24 * 1024;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// Fill `out` with the pattern for file byte offset `at` (8-byte words of a
/// mixed counter, so a misplaced block shows as a mismatch).
fn fill(out: &mut [u8], at: u64) {
    debug_assert!(at % 8 == 0 && out.len() % 8 == 0);
    for (index, word) in out.chunks_exact_mut(8).enumerate() {
        let j = at / 8 + index as u64;
        word.copy_from_slice(&j.wrapping_mul(MIX).to_le_bytes());
    }
}

fn mbps(bytes: usize, elapsed: Duration) -> f64 {
    bytes as f64 / MIB as f64 / elapsed.as_secs_f64().max(1e-9)
}

fn must<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|error| fail(NAME, &format!("{what}: {error}")))
}

/// Write the big file in `IO`-sized calls and `fsync` it.
fn write_big() {
    let mut file = must(File::create(BIG), "create big");
    let mut buf = vec![0u8; IO];
    let start = Instant::now();
    for chunk in 0..BIG_MIB {
        fill(&mut buf, (chunk * IO) as u64);
        must(file.write_all(&buf), "write big");
    }
    let written = start.elapsed();
    must(file.sync_all(), "fsync big");
    let synced = start.elapsed();
    println!(
        "DISK:write_mbps:{:.1} MB/s buffered={:.1} MB/s fsync_ms={}",
        mbps(BIG_MIB * MIB, synced),
        mbps(BIG_MIB * MIB, written),
        (synced - written).as_millis()
    );
}

/// Read the big file sequentially and check every byte.
fn read_big(pass_name: &str) {
    let mut file = must(File::open(BIG), "open big");
    let mut buf = vec![0u8; IO];
    let mut want = vec![0u8; IO];
    let mut total = 0usize;
    let mut busy = Duration::ZERO;
    loop {
        let start = Instant::now();
        let read = must(file.read(&mut buf), "read big");
        busy += start.elapsed();
        if read == 0 {
            break;
        }
        // Reads may be short; compare what came back against its offset.
        let aligned = read - read % 8;
        fill(&mut want[..aligned], total as u64);
        if buf[..aligned] != want[..aligned] || aligned != read {
            fail(NAME, &format!("{pass_name}: wrong bytes near offset {total}"));
        }
        total += read;
    }
    if total != BIG_MIB * MIB {
        fail(NAME, &format!("{pass_name}: read {total} bytes"));
    }
    println!("DISK:{pass_name}_mbps:{:.1} MB/s", mbps(total, busy));
}

/// Read BusyBox whole a few times (it is cached after the first).
fn read_binary() {
    let start = Instant::now();
    let mut first = Vec::new();
    for round in 0..5 {
        let data = must(fs::read(BUSYBOX), "read busybox");
        if round == 0 {
            first = data;
        } else if data != first {
            fail(NAME, "busybox read back differently");
        }
    }
    println!(
        "DISK:read_binary_mbps:{:.1} MB/s size={}",
        mbps(first.len() * 5, start.elapsed()),
        first.len()
    );
}

/// Spawn `busybox true` and wait for it, `EXECS` times.
fn exec_busybox() {
    let mut times = Vec::with_capacity(EXECS);
    for _ in 0..EXECS {
        let start = Instant::now();
        let status = must(Command::new(BUSYBOX).arg("true").status(), "spawn busybox");
        times.push(start.elapsed());
        if !status.success() {
            fail(NAME, &format!("busybox true exited {status}"));
        }
    }
    times.sort();
    let mean = times.iter().sum::<Duration>() / EXECS as u32;
    println!(
        "DISK:exec_ms:{:.2} ms min={:.2} median={:.2} max={:.2}",
        mean.as_secs_f64() * 1e3,
        times[0].as_secs_f64() * 1e3,
        times[EXECS / 2].as_secs_f64() * 1e3,
        times[EXECS - 1].as_secs_f64() * 1e3
    );
}

/// Write many small files in a few directories, then `fsync` one of them
/// (which commits the volume), like a package install.
fn small_files() {
    let mut buf = vec![0u8; SMALL_BYTES];
    let start = Instant::now();
    let mut last = None;
    for index in 0..SMALL_FILES {
        let dir = format!("{DIR}/pkg{}", index % 8);
        if index < 8 {
            must(fs::create_dir_all(&dir), "mkdir pkg");
        }
        fill(&mut buf, (index * SMALL_BYTES) as u64);
        let path = format!("{dir}/f{index}");
        let mut file = must(
            OpenOptions::new().create(true).write(true).truncate(true).open(&path),
            "create small",
        );
        must(file.write_all(&buf), "write small");
        last = Some(file);
    }
    if let Some(file) = last {
        must(file.sync_all(), "fsync small");
    }
    let elapsed = start.elapsed();
    println!(
        "DISK:small_files_ms:{} ms files={SMALL_FILES} bytes={}",
        elapsed.as_millis(),
        SMALL_FILES * SMALL_BYTES
    );
    for index in (0..SMALL_FILES).step_by(37) {
        let path = format!("{DIR}/pkg{}/f{index}", index % 8);
        let data = must(fs::read(&path), "read small");
        fill(&mut buf, (index * SMALL_BYTES) as u64);
        if data != buf {
            fail(NAME, &format!("{path} read back differently"));
        }
    }
}

fn main() {
    let _ = fs::remove_dir_all(DIR);
    must(fs::create_dir_all(DIR), "mkdir bench");
    write_big();
    read_big("read_cold");
    read_big("read_again");
    read_binary();
    exec_busybox();
    small_files();
    must(fs::remove_dir_all(DIR), "remove bench");
    pass(NAME);
    // Leave the kernel's periodic `PERF:` report (every 2 s) time to print
    // the interrupts-off stretches of the run.
    std::thread::sleep(Duration::from_millis(4500));
    println!("DISK:done");
}

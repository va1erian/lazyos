//! Launches the built disk image in QEMU.
//!
//! ```text
//! cargo run                                   interactive window, serial on stdio
//! cargo run -- --headless                     no window, serial only
//! cargo run -- --headless --expect "LazyOS: scheduler started" --timeout 120
//! ```
//!
//! With `--headless` the guest's serial output is streamed to stdout. Each
//! `--expect <text>` must appear in it, or the run fails; `--timeout <secs>`
//! (default 180) kills a guest that never finishes. The guest can end the run
//! itself through `isa-debug-exit` (the kernel test build does after its
//! summary), which sets the exit code below.

use std::env;
use std::io::{BufRead, BufReader};
use std::process::{exit, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Path produced by `build.rs`.
const BIOS_IMAGE: &str = env!("BIOS_IMAGE");

/// QEMU's exit status is `(value << 1) | 1` for the value the guest writes to
/// the `isa-debug-exit` port: 0x10 (success) and 0x11 (failure).
const GUEST_SUCCESS: i32 = (0x10 << 1) | 1;
const GUEST_FAILURE: i32 = (0x11 << 1) | 1;

const DEFAULT_TIMEOUT_SECS: u64 = 180;

struct Options {
    headless: bool,
    expect: Vec<String>,
    timeout: Duration,
}

fn parse_args() -> Options {
    let mut options = Options {
        headless: false,
        expect: Vec::new(),
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => options.headless = true,
            "--expect" => options
                .expect
                .push(args.next().expect("--expect needs text")),
            "--timeout" => {
                let secs = args.next().and_then(|v| v.parse().ok());
                options.timeout = Duration::from_secs(secs.expect("--timeout needs seconds"));
            }
            other => eprintln!("ignoring unknown argument: {other}"),
        }
    }
    options
}

fn qemu_command(headless: bool) -> Command {
    let qemu = env::var("QEMU").unwrap_or_else(|_| "qemu-system-x86_64".to_string());
    let mut cmd = Command::new(qemu);
    cmd.arg("-drive")
        .arg(format!("format=raw,file={BIOS_IMAGE}"));
    cmd.arg("-m").arg("256M");
    cmd.arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
    if headless {
        cmd.arg("-display").arg("none");
        cmd.arg("-serial").arg("stdio");
    } else {
        cmd.arg("-serial").arg("mon:stdio");
    }
    cmd
}

/// Exit code for a QEMU status: 0 when the guest reported success (or QEMU
/// exited cleanly), 1 when it reported failure, 2 for anything else.
fn verdict(code: Option<i32>) -> i32 {
    match code {
        Some(0) | Some(GUEST_SUCCESS) => 0,
        Some(GUEST_FAILURE) => 1,
        _ => 2,
    }
}

fn main() {
    let options = parse_args();
    let mut cmd = qemu_command(options.headless);
    if !options.headless {
        let status = cmd.status().expect("failed to start qemu-system-x86_64");
        exit(verdict(status.code()));
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to start qemu-system-x86_64");
    let stdout = child.stdout.take().expect("piped stdout");
    let (lines, received) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + options.timeout;
    let mut serial = String::new();
    let mut timed_out = false;
    loop {
        match received.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                println!("{line}");
                serial.push_str(&line);
                serial.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Stop early once every expectation is met and the guest is idle.
        let satisfied =
            !options.expect.is_empty() && options.expect.iter().all(|e| serial.contains(e));
        if satisfied || Instant::now() >= deadline {
            timed_out = !satisfied;
            let _ = child.kill();
            break;
        }
    }
    let status = child.wait().expect("failed to wait for qemu");

    let missing: Vec<&String> = options
        .expect
        .iter()
        .filter(|e| !serial.contains(*e))
        .collect();
    for text in &missing {
        eprintln!("missing expected serial output: {text:?}");
    }
    if timed_out {
        eprintln!("timed out after {:?}", options.timeout);
    }
    if !missing.is_empty() || timed_out {
        exit(1);
    }
    // A killed guest (all expectations met) has no meaningful status.
    exit(if options.expect.is_empty() {
        verdict(status.code())
    } else {
        0
    });
}

//! `lazyvm` — run the command list of `tools/lazyvm/run.py` and report each
//! command's stdout, stderr and exit status over the serial log, byte exact.
//!
//! The image (`LAZYOS_LAZYVM=1`, `LAZYOS_LAZYVM_COMMANDS`) holds LazyVM's stage
//! under `fhs::LAZYVM` and the list at `fhs::LAZYVM_COMMANDS`: one command per
//! line, `<timeout seconds> <argv[0] hex> <argv[1] hex> ...` (hex, so no
//! argument needs quoting). Each command runs in order with stdin closed, its
//! stdout and stderr on separate pipes; one that outlives its timeout is
//! killed. The lines, parsed by `tools/lazyvm/judge.py`:
//!
//! - `LAZYVM:INIT:<count>` once the list is read;
//! - `LAZYVM:CMD:<n>:<line hex>`, the list's line, so the host can tell an
//!   image built for another list;
//! - `LAZYVM:DATA:<n>:<out|err>:<seq>:<hex>`, the output in chunks;
//! - `LAZYVM:SPAWN:<n>:<reason>` when the program cannot start (exit 127);
//! - `LAZYVM:END:<n>:<exit>:<stdout bytes>:<stderr bytes>:<timed out 0|1>`,
//!   where a signal is reported as 128 + its number, a timeout as 124;
//! - `LAZYVM:DONE` after the last, or `LAZYVM:FAIL:<reason>`.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Output bytes per `DATA` line (twice as many hex digits).
const CHUNK: usize = 256;

fn main() {
    let list = match std::fs::read_to_string(fhs::LAZYVM_COMMANDS) {
        Ok(list) => list,
        Err(error) => {
            println!("LAZYVM:FAIL:cannot read {}: {error}", fhs::LAZYVM_COMMANDS);
            std::process::exit(1);
        }
    };
    let lines: Vec<&str> = list.lines().filter(|line| !line.is_empty()).collect();
    println!("LAZYVM:INIT:{}", lines.len());
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        println!("LAZYVM:CMD:{n}:{}", hex(line.as_bytes()));
        match parse(line) {
            Ok((timeout, argv)) => run(n, timeout, &argv),
            Err(reason) => {
                println!("LAZYVM:FAIL:command {n}: {reason}");
                std::process::exit(1);
            }
        }
    }
    println!("LAZYVM:DONE");
}

/// A list line: the timeout and the decoded arguments.
fn parse(line: &str) -> Result<(Duration, Vec<Vec<u8>>), String> {
    let mut words = line.split(' ');
    let seconds: u64 = words
        .next()
        .and_then(|word| word.parse().ok())
        .ok_or("no timeout")?;
    let argv = words.map(unhex).collect::<Result<Vec<_>, _>>()?;
    if argv.is_empty() {
        return Err("no program".into());
    }
    Ok((Duration::from_secs(seconds), argv))
}

/// Run one command and print its markers.
fn run(n: usize, timeout: Duration, argv: &[Vec<u8>]) {
    use std::os::unix::ffi::OsStrExt;
    let arg = |bytes: &Vec<u8>| std::ffi::OsStr::from_bytes(bytes).to_os_string();
    let spawned = Command::new(arg(&argv[0]))
        .args(argv[1..].iter().map(arg))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            println!("LAZYVM:SPAWN:{n}:{error}");
            println!("LAZYVM:END:{n}:127:0:0:0");
            return;
        }
    };
    let (out, err, timed_out) = collect(&mut child, timeout);
    let code = match child.wait() {
        _ if timed_out => 124,
        Ok(status) => exit_code(status),
        Err(_) => 125,
    };
    report(n, "out", &out);
    report(n, "err", &err);
    println!(
        "LAZYVM:END:{n}:{code}:{}:{}:{}",
        out.len(),
        err.len(),
        u8::from(timed_out)
    );
}

/// Read the child's stdout and stderr to their ends, killing it when `timeout`
/// passes first; `(stdout, stderr, timed out)`.
fn collect(child: &mut Child, timeout: Duration) -> (Vec<u8>, Vec<u8>, bool) {
    let (done, finished) = mpsc::channel();
    let readers = [
        reader(
            child.stdout.take().map(|s| Box::new(s) as _),
            0,
            done.clone(),
        ),
        reader(child.stderr.take().map(|s| Box::new(s) as _), 1, done),
    ];
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    for _ in 0..readers.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if finished.recv_timeout(left).is_err() {
            timed_out = true;
            // A killed child closes its pipes, so the readers end too.
            let _ = child.kill();
            break;
        }
    }
    let [out, err] = readers.map(|reader| reader.join().unwrap_or_default());
    (out, err, timed_out)
}

/// A thread reading `pipe` to its end, which sends `which` on `done` then.
fn reader(
    pipe: Option<Box<dyn Read + Send>>,
    which: u8,
    done: mpsc::Sender<u8>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = done.send(which);
        bytes
    })
}

/// The shell's convention: the exit code, or 128 + the signal that ended it.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(125)
}

/// Print `bytes` as `DATA` lines.
fn report(n: usize, stream: &str, bytes: &[u8]) {
    for (seq, chunk) in bytes.chunks(CHUNK).enumerate() {
        println!("LAZYVM:DATA:{n}:{stream}:{seq}:{}", hex(chunk));
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("odd hex {text:?}"));
    }
    let digit = |pair: &[u8]| {
        let pair = std::str::from_utf8(pair).ok()?;
        u8::from_str_radix(pair, 16).ok()
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| digit(pair).ok_or_else(|| format!("bad hex {text:?}")))
        .collect()
}

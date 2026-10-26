//! `lazyvm` — run the command list of `tools/lazyvm/run.py` and report each
//! command's stdout, stderr and exit status over the serial log, byte exact.
//!
//! The image (`LAZYOS_LAZYVM=1`, `LAZYOS_LAZYVM_COMMANDS`) holds LazyVM's stage
//! under `fhs::LAZYVM` and the list at `fhs::LAZYVM_COMMANDS`: one command per
//! line, `<timeout seconds> <argv[0] hex> <argv[1] hex> ...` (hex, so no
//! argument needs quoting). Each command runs in order with stdin closed, its
//! stdout and stderr on separate pipes, in a process group of its own; the
//! group is killed when the program outlives its timeout, and when its pipes
//! stay open [`GRACE`] after it exited (a grandchild holding them). Output is
//! sent while it is read, at most [`CAP`] bytes a stream. The lines, parsed by
//! `tools/lazyvm/judge.py`:
//!
//! - `LAZYVM:INIT:<count>` once the list is read;
//! - `LAZYVM:CMD:<n>:<line hex>`, the list's line, so the host can tell an
//!   image built for another list;
//! - `LAZYVM:DATA:<n>:<out|err>:<seq>:<hex>`, the output in chunks;
//! - `LAZYVM:SPAWN:<n>:<reason>` when the program cannot start (exit 127);
//! - `LAZYVM:END:<n>:<exit>:<stdout bytes>:<stderr bytes>:<timed out 0|1>:<truncated 0|1>`,
//!   where a signal is reported as 128 + its number, a timeout as 124 and an
//!   exit status that never came (a failed `wait`, or none [`GRACE`] after the
//!   kill) as 125; truncated is 1 when a stream passed [`CAP`];
//! - `LAZYVM:DONE` after the last, or `LAZYVM:FAIL:<reason>`.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

/// Output bytes per `DATA` line (twice as many hex digits).
const CHUNK: usize = 256;
/// The most output of one stream the host is sent; the rest is read and dropped.
const CAP: usize = 8 << 20;
/// Reads the readers may queue before they wait for the main thread: what a slow serial
/// port holds back stays bounded, and the deadline is checked between any two of them.
const QUEUE: usize = 16;
/// How long the pipes may stay open after the exit, and the exit may take after a kill.
const GRACE: Duration = Duration::from_secs(5);
const SIGKILL: i32 = 9;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// What the reader and waiter threads tell the main one.
enum Event {
    Data(usize, Vec<u8>),
    /// The stream passed [`CAP`]; the reader drops the rest.
    Truncated(usize),
    Eof(usize),
    Exit(Option<ExitStatus>),
}

/// One output stream's progress.
#[derive(Default)]
struct Stream {
    sent: usize,
    seq: usize,
    truncated: bool,
    closed: bool,
}

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
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            println!("LAZYVM:SPAWN:{n}:{error}");
            println!("LAZYVM:END:{n}:127:0:0:0:0");
            return;
        }
    };
    let pid = child.id() as i32;
    let (events, received) = mpsc::sync_channel(QUEUE);
    reader(
        child.stdout.take().map(|s| Box::new(s) as _),
        0,
        events.clone(),
    );
    reader(
        child.stderr.take().map(|s| Box::new(s) as _),
        1,
        events.clone(),
    );
    thread::spawn(move || {
        let _ = events.send(Event::Exit(child.wait().ok()));
    });
    let mut streams = [Stream::default(), Stream::default()];
    let (status, timed_out) = follow(n, pid, timeout, &received, &mut streams);
    let code = match status {
        _ if timed_out => 124,
        Some(Some(status)) => exit_code(status),
        _ => 125,
    };
    let truncated = streams.iter().any(|stream| stream.truncated);
    println!(
        "LAZYVM:END:{n}:{code}:{}:{}:{}:{}",
        streams[0].sent,
        streams[1].sent,
        u8::from(timed_out),
        u8::from(truncated)
    );
}

/// Send the output as it comes until the program exited and both pipes closed. The
/// group is killed at `timeout`, or [`GRACE`] after the exit while a pipe is open; one
/// [`GRACE`] after that kill the threads are left behind. `(exit, timed out)`.
fn follow(
    n: usize,
    pid: i32,
    timeout: Duration,
    received: &mpsc::Receiver<Event>,
    streams: &mut [Stream; 2],
) -> (Option<Option<ExitStatus>>, bool) {
    let mut deadline = Instant::now() + timeout;
    let (mut status, mut timed_out, mut killed) = (None, false, false);
    while status.is_none() || streams.iter().any(|stream| !stream.closed) {
        // Checked on every turn: a stream that never pauses must not hold the deadline off.
        let now = Instant::now();
        if now >= deadline {
            if killed {
                break;
            }
            timed_out = status.is_none();
            kill_group(pid);
            killed = true;
            deadline = now + GRACE;
            continue;
        }
        match received.recv_timeout(deadline - now) {
            Ok(Event::Data(which, bytes)) => send(n, which, &mut streams[which], &bytes),
            Ok(Event::Truncated(which)) => streams[which].truncated = true,
            Ok(Event::Eof(which)) => streams[which].closed = true,
            Ok(Event::Exit(exit)) => {
                status = Some(exit);
                deadline = deadline.min(Instant::now() + GRACE);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    (status, timed_out)
}

/// Kill the program's process group (it leads it), or the program alone if that fails.
fn kill_group(pid: i32) {
    // SAFETY: `kill` takes no pointers; a stale id at worst fails with ESRCH.
    unsafe {
        if kill(-pid, SIGKILL) != 0 {
            kill(pid, SIGKILL);
        }
    }
}

/// A thread sending what `pipe` reads as `Data`, at most [`CAP`] bytes, then one
/// `Truncated` if there was more (read on and dropped, so the program never blocks on a
/// full pipe), then `Eof`. A send fails once the main thread stopped listening.
fn reader(pipe: Option<Box<dyn Read + Send>>, which: usize, events: SyncSender<Event>) {
    thread::spawn(move || {
        let mut buffer = vec![0; CHUNK * 16];
        let (mut sent, mut truncated) = (0, false);
        if let Some(mut pipe) = pipe {
            while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                if truncated {
                    continue;
                }
                let take = read.min(CAP - sent);
                if take > 0 {
                    let data = Event::Data(which, buffer[..take].to_vec());
                    if events.send(data).is_err() {
                        return;
                    }
                    sent += take;
                }
                if take < read {
                    truncated = true;
                    if events.send(Event::Truncated(which)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = events.send(Event::Eof(which));
    });
}

/// Print `bytes` as `DATA` lines of `stream` (the reader keeps it within [`CAP`]).
fn send(n: usize, which: usize, stream: &mut Stream, bytes: &[u8]) {
    let name = ["out", "err"][which];
    for chunk in bytes.chunks(CHUNK) {
        println!("LAZYVM:DATA:{n}:{name}:{}:{}", stream.seq, hex(chunk));
        stream.seq += 1;
        stream.sent += chunk.len();
    }
}

/// The shell's convention: the exit code, or 128 + the signal that ended it.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(125)
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

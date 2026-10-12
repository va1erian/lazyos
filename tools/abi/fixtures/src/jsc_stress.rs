//! Runs the subset of WebKit's `JSTests/stress` that the image carries
//! (`tools/jsc/stress.py`, embedded at `/system/share/jsc-tests/`) through one
//! shell, each file with a time limit, and reports
//! `<prefix>STRESS:<pass>/<total>` plus one `<prefix>STRESS:FAIL:<name>:<reason>`
//! per failure.

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const TESTS: &str = "/system/share/jsc-tests";
/// How long one script may run before it is killed and counted as failed.
const LIMIT: Duration = Duration::from_secs(10);

/// One-line, colon-free text for a marker's reason.
fn tidy(text: &str) -> String {
    text.trim()
        .replace('\n', "\\n")
        .replace(':', ";")
        .chars()
        .take(120)
        .collect()
}

fn drain<R: Read + Send + 'static>(stream: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The first line of `text` that says something.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// Run `program path` for at most [`LIMIT`]; `Err` is the reason it failed.
fn run_limited(program: &str, path: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn {error}"))?;
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let began = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if began.elapsed() >= LIMIT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("wait {error}")),
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if timed_out {
        return Err("timeout".into());
    }
    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => {
            let said = if err.trim().is_empty() {
                first_line(&out)
            } else {
                first_line(&err)
            };
            Err(format!("{status} {}", tidy(said)))
        }
        None => Err("no status".into()),
    }
}

/// Run every listed script with `program`; false only when the list is unreadable.
pub fn run(program: &str, prefix: &str) -> bool {
    let Ok(index) = fs::read_to_string(format!("{TESTS}/index.txt")) else {
        println!("{prefix}STRESS:FAIL:index:no {TESTS}/index.txt (build with tools/jsc/stress.py)");
        return false;
    };
    let began = Instant::now();
    let (mut passed, mut total) = (0usize, 0usize);
    for name in index.lines().filter(|line| !line.is_empty()) {
        total += 1;
        match run_limited(
            &format!("/system/bin/{program}"),
            &format!("{TESTS}/{name}"),
        ) {
            Ok(()) => passed += 1,
            Err(reason) => println!("{prefix}STRESS:FAIL:{name}:{}", tidy(&reason)),
        }
    }
    println!("{prefix}STRESS:{passed}/{total}");
    println!("{prefix}STRESS:TIME:{}", began.elapsed().as_millis());
    total > 0
}

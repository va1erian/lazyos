//! Serial evidence markers (AGENTS.md: a graphics claim needs real output).
//!
//! Each marker is one line for the serial console. When the program is started
//! from a shell (a Terminal window), its stdout is a pipe to that window, not
//! the serial port, so the line is written to `/dev/console` (the kernel
//! terminal, serial included) and only falls back to stdout when that device
//! cannot be opened (a host build, or an unusual sandbox). Each one-shot marker
//! fires at most once per process so a session script can assert on it. The
//! format is `<PREFIX>:<STAGE>:<PASS|FAIL|WARN>[:<detail>]`.

use std::cell::Cell;
use std::io::Write;

/// Emits `PREFIX:STAGE:PASS` or `PREFIX:STAGE:FAIL:detail` lines.
#[derive(Clone, Copy, Debug)]
pub struct Markers {
    prefix: &'static str,
}

impl Markers {
    /// Markers for the `lrplay` player.
    pub const PLAYER: Markers = Markers { prefix: "LRPLAY" };
    /// Markers for the `lazyrad` IDE.
    pub const IDE: Markers = Markers { prefix: "LRIDE" };

    /// The line for a passing `stage`.
    pub fn pass_line(&self, stage: &str) -> String {
        format!("{}:{stage}:PASS", self.prefix)
    }

    /// The line for a failing `stage`; newlines in `detail` are flattened so a
    /// marker is always exactly one line.
    pub fn fail_line(&self, stage: &str, detail: &str) -> String {
        format!("{}:{stage}:FAIL:{}", self.prefix, flatten(detail))
    }

    /// Prints a passing `stage`.
    pub fn pass(&self, stage: &str) {
        emit(&self.pass_line(stage));
    }

    /// Prints a passing `stage` with a `detail` (a path, say), one line.
    pub fn pass_with(&self, stage: &str, detail: &str) {
        emit(&format!("{}:{stage}:PASS:{}", self.prefix, flatten(detail)));
    }

    /// Prints a failing `stage`.
    pub fn fail(&self, stage: &str, detail: &str) {
        emit(&self.fail_line(stage, detail));
    }

    /// The line for a `stage` that works in a degraded way.
    pub fn warn_line(&self, stage: &str, detail: &str) -> String {
        format!("{}:{stage}:WARN:{}", self.prefix, flatten(detail))
    }

    /// Prints a degraded `stage` on the console and on stderr (the Terminal
    /// window that started the program, when there is one).
    pub fn warn(&self, stage: &str, detail: &str) {
        let line = self.warn_line(stage, detail);
        emit(&line);
        eprintln!("{line}");
    }

    /// Reports a panic as a `PANIC` failure marker on the console before the
    /// default hook runs. The program's stderr is usually a pipe to a Terminal
    /// window (or nowhere), so without this a crash leaves no serial evidence.
    pub fn install_panic_hook(self) {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            self.fail("PANIC", &info.to_string());
            // Raw return addresses: the binary is static and not position
            // independent, so they symbolize offline against an unstripped build
            // (`CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1`).
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            for line in trace.lines() {
                emit(&format!("{}:PANIC:BT:{}", self.prefix, line.trim()));
            }
            previous(info);
        }));
    }

    /// A one-shot for `stage`: the returned closure prints the pass line the
    /// first time it is called and does nothing afterwards.
    pub fn once(&self, stage: &'static str) -> impl Fn() {
        let markers = *self;
        let fired = Cell::new(false);
        move || {
            if !fired.replace(true) {
                markers.pass(stage);
            }
        }
    }
}

/// `detail` on one line: control characters become spaces.
fn flatten(detail: &str) -> String {
    detail
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Writes `line` and a newline to the console device, else to stdout.
fn emit(line: &str) {
    let written = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/console")
        .and_then(|mut device| writeln!(device, "{line}"));
    if written.is_err() {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_follow_the_documented_shape() {
        assert_eq!(Markers::PLAYER.pass_line("UP"), "LRPLAY:UP:PASS");
        assert_eq!(Markers::IDE.pass_line("RUN"), "LRIDE:RUN:PASS");
        assert_eq!(
            Markers::PLAYER.fail_line("BIND", "code 2"),
            "LRPLAY:BIND:FAIL:code 2"
        );
    }

    #[test]
    fn a_failure_detail_is_always_one_line() {
        let line = Markers::PLAYER.fail_line("LOAD", "a\nb\r\nc");
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert_eq!(line, "LRPLAY:LOAD:FAIL:a b  c");
        assert_eq!(
            Markers::IDE.warn_line("HOME", "x\ny"),
            "LRIDE:HOME:WARN:x y"
        );
    }
}

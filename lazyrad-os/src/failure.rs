//! Telling `init` why the player failed (issue #549).
//!
//! The player reports a load, compile or fatal runtime problem as one JSON
//! object per line on stderr (`lazyrad_player::Report`), then exits 1 or 2.
//! `init` will not restart an installed app that fails while starting, and
//! the desktop shows the user one notice instead; this module gives that
//! notice its reason. [`StderrTap`] passes stderr through unchanged while
//! remembering the last report line, and [`report_to_init`] hands its text to
//! `init.ReportFailure` (one one-way message, generated stub) just before the
//! player exits.

use lazyrad_player::Report;
use libmessenger::{Header, Parcel, VERSION};
use messenger_generated::os_lazy_init_v1 as init_wire;
use xui_app::sys;

/// `init`'s registered name.
const INIT: &str = "os.lazy.init";
/// The last report line seen, as readable text.
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
fn summarize(line: &str) -> Option<String> {
    Report::from_json(line).map(|report| report.to_text())
}

#[cfg(unix)]
pub use tap::StderrTap;

/// Off LazyOS (the host tools build this library too) there is no tap.
#[cfg(not(unix))]
pub struct StderrTap;

#[cfg(not(unix))]
impl StderrTap {
    pub fn install() -> Option<StderrTap> {
        None
    }

    pub fn finish(self) -> Option<String> {
        None
    }
}

#[cfg(unix)]
mod tap {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::summarize;

    /// stderr.
    const STDERR: RawFd = 2;

    /// stderr, routed through a pipe whose reader keeps the last report.
    pub struct StderrTap {
        saved: OwnedFd,
        reader: Option<JoinHandle<()>>,
        last: Arc<Mutex<Option<String>>>,
    }

    impl StderrTap {
        /// Route fd 2 through the tap; `None` (stderr untouched) when the pipe
        /// or the reader thread cannot be made.
        pub fn install() -> Option<StderrTap> {
            let mut fds = [0 as RawFd; 2];
            // SAFETY: `fds` is a two-element array, what `pipe` writes.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: `pipe` just returned both descriptors; each is owned once.
            let (read, write) =
                unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            // SAFETY: `dup` of the open stderr; the result is owned here.
            let saved = unsafe { libc::dup(STDERR) };
            if saved < 0 {
                return None;
            }
            // SAFETY: `saved` is the fresh descriptor `dup` returned.
            let saved = unsafe { OwnedFd::from_raw_fd(saved) };
            let echo = saved.try_clone().ok()?;
            let last = Arc::new(Mutex::new(None));
            let seen = Arc::clone(&last);
            let reader = std::thread::Builder::new()
                .name("stderr-tap".into())
                .spawn(move || pump(read, echo, &seen))
                .ok()?;
            // SAFETY: both descriptors are open; `dup2` replaces fd 2, and the
            // pipe's write end stays open through it after `write` drops.
            if unsafe { libc::dup2(write.as_raw_fd(), STDERR) } < 0 {
                return None;
            }
            drop(write);
            Some(StderrTap {
                saved,
                reader: Some(reader),
                last,
            })
        }

        /// Put stderr back, let the reader see the end, and return the last
        /// report's text.
        pub fn finish(mut self) -> Option<String> {
            let _ = std::io::stderr().flush();
            // SAFETY: `saved` is the original stderr; `dup2` closes the pipe's
            // last write end in fd 2, which ends the reader.
            unsafe { libc::dup2(self.saved.as_raw_fd(), STDERR) };
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            self.last.lock().ok()?.take()
        }
    }

    /// Copy the pipe to the real stderr line by line, remembering the last
    /// report line.
    fn pump(read: OwnedFd, echo: OwnedFd, last: &Mutex<Option<String>>) {
        use std::io::{BufRead, BufReader};
        let mut out = std::fs::File::from(echo);
        let reader = BufReader::new(std::fs::File::from(read));
        for line in reader.split(b'\n').map_while(Result::ok) {
            let _ = out.write_all(&line);
            let _ = out.write_all(b"\n");
            if let Some(text) = summarize(&String::from_utf8_lossy(&line)) {
                if let Ok(mut slot) = last.lock() {
                    *slot = Some(text);
                }
            }
        }
    }
}

/// Tell `init` why this run fails (`ReportFailure`, one way). Best effort: a
/// player started outside `init` is simply not listened to.
pub fn report_to_init(reason: &str) {
    let Ok(body) = init_wire::encode_report_failure_args(&init_wire::ReportFailureArgs {
        reason: reason.to_owned(),
    }) else {
        return;
    };
    let Ok(endpoint) = sys::msg_resolve(INIT) else {
        return;
    };
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: libmessenger::flags::ONE_WAY,
            interface_id: init_wire::INTERFACE_ID,
            method: init_wire::METHOD_REPORTFAILURE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    let _ = sys::msg_send(endpoint, &parcel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_report_lines_become_reasons() {
        let line =
            r#"{"kind":"runtime","file":"main_form.rhai","line":3,"col":7,"message":"boom"}"#;
        assert_eq!(summarize(line).as_deref(), Some("main_form.rhai:3:7: boom"));
        assert_eq!(summarize("LRPLAY:RUN:FAIL:exit code 2"), None);
        assert_eq!(summarize(""), None);
    }
}

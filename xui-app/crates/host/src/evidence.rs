//! The process's evidence sink, for a crate whose evidence sites are free
//! functions and worker-thread answers with no host in reach (`settings`,
//! `confd-editor`).
//!
//! A desktop app is one process with one evidence stream, so the host
//! installs one sink at start-up (`AppContext` turns each line into a `trace`
//! line on the app's channel) and the portable crates call [`evidence!`]. A
//! crate that has an [`AppHost`](crate::AppHost) to hand reports through it
//! instead; both reach the same `trace`. Without a sink (a host test that
//! installed none) a line is dropped; tests read them with [`capture`].

use std::sync::{Mutex, OnceLock};

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();
static CAPTURED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Installs the process's sink; the first call wins. Returns whether this
/// one was installed.
pub fn install(sink: impl Fn(&str) + Send + Sync + 'static) -> bool {
    SINK.set(Box::new(sink)).is_ok()
}

/// One evidence line, without its channel prefix.
pub fn emit(line: &str) {
    if let Some(captured) = CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        captured.push(line.to_owned());
    }
    if let Some(sink) = SINK.get() {
        sink(line);
    }
}

/// Starts keeping what is emitted (for a test), forgetting what was kept.
pub fn capture() {
    *CAPTURED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
}

/// What was emitted since [`capture`].
pub fn captured() -> Vec<String> {
    CAPTURED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// `evidence!("LAYOUT:APPLIED:{name}")`: formats and emits one line.
#[macro_export]
macro_rules! evidence {
    ($($line:tt)+) => {
        $crate::evidence::emit(&format!($($line)+))
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_captured_line_is_kept_without_a_sink() {
        super::capture();
        crate::evidence!("CLOSE:{}", "PASS");
        assert!(super::captured().contains(&"CLOSE:PASS".to_owned()));
    }
}

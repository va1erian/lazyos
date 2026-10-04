//! The Terminal's own latency evidence, one serial line per submitted
//! command (never per keystroke, so measuring costs nothing on the path it
//! measures):
//!
//! * `TERM:PERF:echo n=<keys> p50_us=<..> max_us=<..>`: from sending a typed
//!   character to the pty to reading its echo back, for the command's keys;
//! * `TERM:PERF:cmd ms=<..> bytes=<..> paints=<..> paint_us=<..>`: from Enter
//!   to the shell's next prompt, the output bytes read meanwhile, and the
//!   frames painted for them with the time the painter spent.

use std::cell::Cell;

use xui_app::sys::monotonic_ns;

/// Painter time, shared with the paint closure.
#[derive(Default)]
pub struct PaintClock {
    paints: Cell<u64>,
    nanos: Cell<u64>,
}

impl PaintClock {
    /// Run one paint and book its duration.
    pub fn time<R>(&self, paint: impl FnOnce() -> R) -> R {
        let start = monotonic_ns();
        let out = paint();
        self.paints.set(self.paints.get() + 1);
        self.nanos
            .set(self.nanos.get() + monotonic_ns().saturating_sub(start));
        out
    }

    fn take(&self) -> (u64, u64) {
        (self.paints.replace(0), self.nanos.replace(0))
    }
}

#[derive(Default)]
pub struct Perf {
    /// When the oldest character not yet echoed was sent.
    echo_pending: Option<u64>,
    /// Echo delays of this command's keys, in microseconds.
    echoes: Vec<u64>,
    /// When the running command was submitted, and its output so far.
    command: Option<u64>,
    bytes: usize,
}

impl Perf {
    /// A typed character went to the pty.
    pub fn sent_key(&mut self) {
        self.echo_pending.get_or_insert_with(monotonic_ns);
    }

    /// Enter went to the pty: report the echoes and start timing the command.
    pub fn submitted(&mut self, paint: &PaintClock) {
        if !self.echoes.is_empty() {
            self.echoes.sort_unstable();
            let p50 = self.echoes[self.echoes.len() / 2];
            let max = self.echoes[self.echoes.len() - 1];
            println!(
                "TERM:PERF:echo n={} p50_us={p50} max_us={max}",
                self.echoes.len()
            );
            self.echoes.clear();
        }
        self.echo_pending = None;
        paint.take();
        self.command = Some(monotonic_ns());
        self.bytes = 0;
    }

    /// `bytes` were read from the pty; `at_prompt` when the shell's prompt
    /// is now on the cursor row.
    pub fn read(&mut self, bytes: usize, at_prompt: bool, paint: &PaintClock) {
        let now = monotonic_ns();
        if let Some(sent) = self.echo_pending.take() {
            self.echoes.push(now.saturating_sub(sent) / 1000);
        }
        self.bytes += bytes;
        if let (true, Some(start)) = (at_prompt, self.command) {
            let (paints, nanos) = paint.take();
            println!(
                "TERM:PERF:cmd ms={} bytes={} paints={paints} paint_us={}",
                now.saturating_sub(start) / 1_000_000,
                self.bytes,
                nanos / 1000
            );
            self.command = None;
        }
    }
}

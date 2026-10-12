//! The player's end of the debug protocol on LazyOS: a non-blocking stdin.
//!
//! LazyRAD's own `--debug` transport reads stdin on a helper thread. LazyOS
//! threads do not share a descriptor table, so that thread would lose the pipe
//! (the same finding as the IDE's pipe readers, `launcher`); the player
//! therefore installs this transport instead, through
//! `lazyrad_player::debug::install_transport`, before it starts.
//!
//! [`StdinTransport`] reads from the **calling thread** and never waits in the
//! kernel: stdin is put in non-blocking mode, `poll` takes what is there and
//! `wait` loops on `poll` with a short sleep. The debug callback calls `poll`
//! every few hundred script nodes, so Pause and new breakpoints reach a running
//! script, and `wait` is what a stopped program sits in. End of input (the IDE
//! closed the pipe or died) is `Closed`, which makes a stopped player resume and
//! run on with the debugger off instead of hanging.
//!
//! The reader and writer are generic so the line handling is tested without a
//! pipe.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};

use lazyrad_runtime::debug::decode_incoming;
use lazyrad_runtime::proto::{encode_event, Event, Line, LineReader};
use lazyrad_runtime::{DebugTransport, Incoming};

/// How long `wait` sleeps between looks at the pipe, in milliseconds.
pub const WAIT_SLEEP_MS: u64 = 10;
/// The most bytes one `poll` reads, so a flood of input cannot starve the
/// script it interrupts.
const READ_BUDGET: usize = 64 * 1024;

/// Commands from `reader` (non-blocking), events to `writer`. See the module
/// documentation.
pub struct StdinTransport<R: Read, W: Write> {
    reader: RefCell<R>,
    writer: RefCell<W>,
    lines: RefCell<LineReader>,
    ready: RefCell<VecDeque<Incoming>>,
    closed: Cell<bool>,
    sleep: fn(u64),
}

impl StdinTransport<std::io::Stdin, std::io::Stdout> {
    /// The transport over this process's stdin and stdout, with stdin switched
    /// to non-blocking mode.
    pub fn stdio() -> Self {
        #[cfg(unix)]
        set_stdin_nonblocking();
        StdinTransport::new(
            std::io::stdin(),
            std::io::stdout(),
            xui_app::sys::sleep_millis,
        )
    }
}

/// Puts descriptor 0 into non-blocking mode.
#[cfg(unix)]
fn set_stdin_nonblocking() {
    // SAFETY: `fcntl(F_SETFL)` on descriptor 0, which this process owns for its
    // whole life; it only changes the file status flags and touches no memory.
    unsafe {
        let flags = libc::fcntl(0, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

impl<R: Read, W: Write> StdinTransport<R, W> {
    /// A transport over `reader` and `writer`; `sleep` waits for the given
    /// milliseconds.
    pub fn new(reader: R, writer: W, sleep: fn(u64)) -> Self {
        StdinTransport {
            reader: RefCell::new(reader),
            writer: RefCell::new(writer),
            lines: RefCell::new(LineReader::new()),
            ready: RefCell::new(VecDeque::new()),
            closed: Cell::new(false),
            sleep,
        }
    }

    /// Reads what the pipe has (up to [`READ_BUDGET`]) into complete commands.
    fn fill(&self) {
        if self.closed.get() {
            return;
        }
        let mut reader = self.reader.borrow_mut();
        let mut lines = self.lines.borrow_mut();
        let mut ready = self.ready.borrow_mut();
        let mut chunk = [0u8; 4096];
        let mut budget = READ_BUDGET;
        while budget > 0 {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    self.closed.set(true);
                    if let Some(line) = lines.finish() {
                        ready.extend(incoming(line));
                    }
                    return;
                }
                Ok(read) => {
                    budget = budget.saturating_sub(read);
                    for line in lines.push(&chunk[..read]) {
                        ready.extend(incoming(line));
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    self.closed.set(true);
                    return;
                }
            }
        }
    }
}

/// One complete line as an incoming item; a blank line is nothing.
fn incoming(line: Line) -> Option<Incoming> {
    match line {
        Line::Text(text) => decode_incoming(&text),
        Line::Oversized => Some(Incoming::Rejected(
            "a command line was too long and was dropped".to_owned(),
        )),
    }
}

impl<R: Read, W: Write> DebugTransport for StdinTransport<R, W> {
    fn send(&self, event: &Event) {
        let line = encode_event(event);
        let mut writer = self.writer.borrow_mut();
        // The IDE going away is noticed on the next read; a failed write is
        // dropped here.
        let _ = writeln!(writer, "{line}").and_then(|()| writer.flush());
    }

    fn poll(&self) -> Incoming {
        self.fill();
        if let Some(item) = self.ready.borrow_mut().pop_front() {
            return item;
        }
        if self.closed.get() {
            Incoming::Closed
        } else {
            Incoming::Empty
        }
    }

    fn wait(&self) -> Incoming {
        loop {
            match self.poll() {
                Incoming::Empty => (self.sleep)(WAIT_SLEEP_MS),
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    use lazyrad_runtime::proto::{encode_command, Command};

    use super::*;

    /// A reader that plays back scripted results; `None` (and an exhausted
    /// script) is `WouldBlock`, as a quiet non-blocking pipe answers.
    struct Script(VecDeque<Option<io::Result<Vec<u8>>>>);

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front() {
                Some(Some(Ok(data))) => {
                    buf[..data.len()].copy_from_slice(&data);
                    Ok(data.len())
                }
                Some(Some(Err(error))) => Err(error),
                Some(None) | None => Err(io::ErrorKind::WouldBlock.into()),
            }
        }
    }

    #[derive(Clone, Default)]
    struct Out(Rc<RefCell<Vec<u8>>>);

    impl Write for Out {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn no_sleep(_: u64) {}

    fn transport(items: Vec<Option<io::Result<Vec<u8>>>>) -> StdinTransport<Script, Out> {
        StdinTransport::new(Script(items.into()), Out::default(), no_sleep)
    }

    fn line(command: &Command) -> Vec<u8> {
        let mut bytes = encode_command(command).into_bytes();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn a_quiet_pipe_is_empty_not_closed() {
        let transport = transport(vec![None]);
        assert_eq!(transport.poll(), Incoming::Empty);
        assert_eq!(transport.poll(), Incoming::Empty);
    }

    #[test]
    fn commands_split_across_reads_are_reassembled() {
        let bytes = line(&Command::Continue);
        let (first, second) = bytes.split_at(7);
        let transport = transport(vec![
            Some(Ok(first.to_vec())),
            None,
            Some(Ok(second.to_vec())),
        ]);
        assert_eq!(transport.poll(), Incoming::Empty, "half a command waits");
        assert_eq!(transport.poll(), Incoming::Command(Command::Continue));
    }

    #[test]
    fn several_commands_in_one_read_come_out_in_order() {
        let mut bytes = line(&Command::Pause);
        bytes.extend(line(&Command::Stop));
        let transport = transport(vec![Some(Ok(bytes))]);
        assert_eq!(transport.poll(), Incoming::Command(Command::Pause));
        assert_eq!(transport.poll(), Incoming::Command(Command::Stop));
    }

    #[test]
    fn end_of_input_is_reported_after_the_last_command() {
        let mut bytes = line(&Command::Pause);
        bytes.extend_from_slice(encode_command(&Command::Stop).as_bytes());
        // The last command has no newline, then the pipe closes.
        let transport = transport(vec![Some(Ok(bytes)), Some(Ok(Vec::new()))]);
        assert_eq!(transport.poll(), Incoming::Command(Command::Pause));
        assert_eq!(transport.poll(), Incoming::Command(Command::Stop));
        assert_eq!(transport.poll(), Incoming::Closed);
        assert_eq!(transport.poll(), Incoming::Closed, "and it stays closed");
    }

    #[test]
    fn a_hard_read_error_closes_like_end_of_input() {
        let transport = transport(vec![Some(Err(io::ErrorKind::BrokenPipe.into()))]);
        assert_eq!(transport.poll(), Incoming::Closed);
    }

    #[test]
    fn an_interrupted_read_is_retried() {
        let transport = transport(vec![
            Some(Err(io::ErrorKind::Interrupted.into())),
            Some(Ok(line(&Command::Continue))),
        ]);
        assert_eq!(transport.poll(), Incoming::Command(Command::Continue));
    }

    #[test]
    fn bad_input_is_rejected_and_the_next_command_still_arrives() {
        let mut bytes = b"garbage\n\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        bytes.extend(line(&Command::Continue));
        let transport = transport(vec![Some(Ok(bytes))]);
        assert!(matches!(transport.poll(), Incoming::Rejected(_)));
        assert!(matches!(transport.poll(), Incoming::Rejected(_)));
        assert_eq!(transport.poll(), Incoming::Command(Command::Continue));
    }

    #[test]
    fn wait_sleeps_between_looks_and_returns_the_command() {
        thread_local! { static SLEPT: Cell<u32> = const { Cell::new(0) }; }
        fn counting_sleep(_: u64) {
            SLEPT.with(|slept| slept.set(slept.get() + 1));
        }
        let transport = StdinTransport::new(
            Script(vec![None, None, None, Some(Ok(line(&Command::Continue)))].into()),
            Out::default(),
            counting_sleep,
        );
        assert_eq!(transport.wait(), Incoming::Command(Command::Continue));
        assert_eq!(SLEPT.with(Cell::get), 3, "one sleep per quiet look");
    }

    #[test]
    fn wait_on_a_closed_pipe_returns_instead_of_hanging() {
        let transport = transport(vec![Some(Ok(Vec::new()))]);
        assert_eq!(transport.wait(), Incoming::Closed);
    }

    #[test]
    fn events_are_one_flushed_line_each_and_a_dead_pipe_is_survived() {
        let out = Out::default();
        let transport = StdinTransport::new(Script(VecDeque::new()), out.clone(), no_sleep);
        transport.send(&Event::Resumed);
        transport.send(&Event::Exited { code: 0 });
        let text = String::from_utf8(out.0.borrow().clone()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|l| l.starts_with('{') && l.ends_with('}')));

        let dead = StdinTransport::new(Script(VecDeque::new()), Broken, no_sleep);
        dead.send(&Event::Resumed); // must not panic
    }

    #[test]
    fn one_poll_reads_a_bounded_amount() {
        let chunks = (0..100).map(|_| Some(Ok(vec![b'a'; 4096]))).collect();
        let transport = transport(chunks);
        let _ = transport.poll();
        let left = transport.reader.borrow().0.len();
        assert_eq!(
            left,
            100 - READ_BUDGET / 4096,
            "the budget stopped the loop"
        );
    }
}

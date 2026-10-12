//! Writing a child's stdin without ever blocking the UI thread.
//!
//! The debugger sends the player one JSON line per command. The launchers here
//! cannot block on that write (they run on the window's timer, and LazyOS
//! threads do not share a descriptor table, so a writer thread would lose the
//! pipe just as the readers do), so the pipe's write end is non-blocking and
//! this queue sits in front of it:
//!
//! * [`InputQueue::push_line`] appends the line to a byte queue and tries to
//!   write at once;
//! * what the pipe does not take (a full pipe, `EAGAIN`/`WouldBlock`) stays
//!   queued, a partial write keeps its tail, and [`InputQueue::flush`] (called
//!   from `ChildProcess::poll`) tries again on every tick;
//! * a hard error (`EPIPE`: the child closed its stdin or died) closes the
//!   queue for good: what was pending is dropped and later lines are refused
//!   with [`SendError::Closed`], never retried;
//! * the queue is bounded, so a child that never reads cannot make the IDE
//!   grow without limit: a line that does not fit is refused with
//!   [`SendError::Full`].
//!
//! Lines pushed before a writer exists (the Play run waits for the Installer's
//! approval before the player starts) are held and written when
//! [`InputQueue::attach`] gives the queue its pipe.

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};

use lazyrad_ide::run::SendError;

/// The most bytes held for a child that is not reading.
pub const QUEUE_LIMIT: usize = 256 * 1024;

/// A byte queue in front of a non-blocking writer. See the module
/// documentation.
pub struct InputQueue<W: Write> {
    writer: Option<W>,
    pending: VecDeque<u8>,
    closed: bool,
}

impl<W: Write> Default for InputQueue<W> {
    fn default() -> InputQueue<W> {
        InputQueue::new()
    }
}

impl<W: Write> InputQueue<W> {
    /// A queue with no pipe yet.
    pub fn new() -> InputQueue<W> {
        InputQueue {
            writer: None,
            pending: VecDeque::new(),
            closed: false,
        }
    }

    /// Gives the queue its pipe and writes what was held. Does nothing for a
    /// queue that already closed (the writer is dropped, closing the pipe).
    pub fn attach(&mut self, writer: W) {
        if self.closed {
            return;
        }
        self.writer = Some(writer);
        self.flush();
    }

    /// Whether the pipe is gone for good.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// How many bytes wait for the pipe.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Queues `line` and a newline and writes what the pipe will take.
    ///
    /// `Err` means the line was *not* queued: the queue is closed, or full.
    pub fn push_line(&mut self, line: &str) -> Result<(), SendError> {
        if self.closed {
            return Err(SendError::Closed);
        }
        // A line cannot be split across a refusal, or the child would read half
        // a command.
        if self.pending.len() + line.len() + 1 > QUEUE_LIMIT {
            return Err(SendError::Full);
        }
        self.pending.extend(line.as_bytes());
        self.pending.push_back(b'\n');
        self.flush();
        if self.closed {
            Err(SendError::Closed)
        } else {
            Ok(())
        }
    }

    /// Writes as much of the queue as the pipe takes now.
    pub fn flush(&mut self) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        while !self.pending.is_empty() {
            // `VecDeque` may be two slices; write the first and loop.
            let (front, _) = self.pending.as_slices();
            match writer.write(front) {
                // A pipe that accepts nothing and reports no error is closed.
                Ok(0) => {
                    self.close();
                    return;
                }
                Ok(written) => {
                    self.pending.drain(..written);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                // BrokenPipe, or anything else the pipe cannot recover from.
                Err(_) => {
                    self.close();
                    return;
                }
            }
        }
        // Nothing for the child to see until the bytes leave the process.
        let flushed = match self.writer.as_mut() {
            Some(writer) => writer.flush(),
            None => Ok(()),
        };
        if let Err(error) = flushed {
            if error.kind() != ErrorKind::WouldBlock && error.kind() != ErrorKind::Interrupted {
                self.close();
            }
        }
    }

    fn close(&mut self) {
        self.closed = true;
        self.pending.clear();
        // Dropping the writer closes our end, which the child sees as EOF.
        self.writer = None;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    use super::*;

    /// What the fake pipe does with the next `write`.
    #[derive(Clone, Copy, Debug)]
    enum Step {
        /// Accept at most this many bytes.
        Take(usize),
        WouldBlock,
        Interrupted,
        BrokenPipe,
        /// `Ok(0)`, as a pipe with no reader sometimes answers.
        Zero,
    }

    #[derive(Default)]
    struct Shared {
        written: Vec<u8>,
        script: VecDeque<Step>,
        dropped: bool,
    }

    struct FakePipe(Rc<RefCell<Shared>>);

    impl Write for FakePipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut shared = self.0.borrow_mut();
            match shared.script.pop_front().unwrap_or(Step::Take(usize::MAX)) {
                Step::Take(limit) => {
                    let count = buf.len().min(limit);
                    shared.written.extend_from_slice(&buf[..count]);
                    Ok(count)
                }
                Step::WouldBlock => Err(io::ErrorKind::WouldBlock.into()),
                Step::Interrupted => Err(io::ErrorKind::Interrupted.into()),
                Step::BrokenPipe => Err(io::ErrorKind::BrokenPipe.into()),
                Step::Zero => Ok(0),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for FakePipe {
        fn drop(&mut self) {
            self.0.borrow_mut().dropped = true;
        }
    }

    fn queue(script: &[Step]) -> (InputQueue<FakePipe>, Rc<RefCell<Shared>>) {
        let shared = Rc::new(RefCell::new(Shared {
            script: script.iter().copied().collect(),
            ..Shared::default()
        }));
        let mut queue = InputQueue::new();
        queue.attach(FakePipe(Rc::clone(&shared)));
        (queue, shared)
    }

    fn written(shared: &Rc<RefCell<Shared>>) -> String {
        String::from_utf8(shared.borrow().written.clone()).unwrap()
    }

    #[test]
    fn a_line_is_written_with_its_newline_at_once() {
        let (mut queue, shared) = queue(&[]);
        queue.push_line("{\"cmd\":1}").unwrap();
        assert_eq!(written(&shared), "{\"cmd\":1}\n");
        assert_eq!(queue.pending(), 0);
    }

    #[test]
    fn a_full_pipe_keeps_the_line_for_the_next_tick() {
        let (mut queue, shared) = queue(&[Step::WouldBlock]);
        queue.push_line("one").unwrap();
        assert_eq!(written(&shared), "", "EAGAIN wrote nothing");
        assert_eq!(queue.pending(), 4);
        assert!(!queue.is_closed(), "EAGAIN is not an error");
        queue.flush();
        assert_eq!(written(&shared), "one\n");
        assert_eq!(queue.pending(), 0);
    }

    #[test]
    fn a_partial_write_keeps_its_tail_and_order() {
        let (mut queue, shared) = queue(&[Step::Take(2), Step::WouldBlock, Step::WouldBlock]);
        queue.push_line("abcdef").unwrap();
        assert_eq!(written(&shared), "ab");
        queue.push_line("gh").unwrap();
        assert_eq!(written(&shared), "ab", "still blocked");
        queue.flush();
        assert_eq!(
            written(&shared),
            "abcdef\ngh\n",
            "nothing lost or reordered"
        );
    }

    #[test]
    fn an_interrupted_write_is_retried() {
        let (mut queue, shared) = queue(&[Step::Interrupted, Step::Interrupted]);
        queue.push_line("x").unwrap();
        assert_eq!(written(&shared), "x\n");
    }

    #[test]
    fn a_broken_pipe_closes_the_queue_for_good() {
        let (mut queue, shared) = queue(&[Step::BrokenPipe]);
        assert_eq!(queue.push_line("one"), Err(SendError::Closed));
        assert!(queue.is_closed());
        assert_eq!(queue.pending(), 0, "pending bytes are dropped");
        assert!(shared.borrow().dropped, "our end of the pipe is closed");
        assert_eq!(queue.push_line("two"), Err(SendError::Closed));
        queue.flush();
        assert_eq!(written(&shared), "");
    }

    #[test]
    fn a_write_of_zero_bytes_means_closed_not_a_spin() {
        let (mut queue, _) = queue(&[Step::Zero]);
        assert_eq!(queue.push_line("one"), Err(SendError::Closed));
        assert!(queue.is_closed());
    }

    #[test]
    fn a_child_that_never_reads_cannot_grow_the_queue_without_bound() {
        let (mut queue, shared) = queue(&vec![Step::WouldBlock; 100_000]);
        let line = "x".repeat(1000);
        let mut accepted = 0;
        let mut refused = 0;
        for _ in 0..600 {
            match queue.push_line(&line) {
                Ok(()) => accepted += 1,
                Err(SendError::Full) => refused += 1,
                Err(other) => panic!("{other:?}"),
            }
        }
        assert!(refused > 0 && accepted > 0);
        assert!(queue.pending() <= QUEUE_LIMIT);
        assert!(!queue.is_closed(), "full is not closed");
        // Once the child reads again everything accepted arrives whole.
        shared.borrow_mut().script.clear();
        queue.flush();
        assert_eq!(shared.borrow().written.len(), accepted * 1001);
    }

    #[test]
    fn lines_pushed_before_the_pipe_exists_are_written_on_attach() {
        let mut queue: InputQueue<FakePipe> = InputQueue::new();
        queue.push_line("early one").unwrap();
        queue.push_line("early two").unwrap();
        assert_eq!(queue.pending(), 20);
        let shared = Rc::new(RefCell::new(Shared::default()));
        queue.attach(FakePipe(Rc::clone(&shared)));
        assert_eq!(written(&shared), "early one\nearly two\n");
    }

    #[test]
    fn attaching_to_a_closed_queue_drops_the_pipe() {
        let (mut queue, _) = queue(&[Step::BrokenPipe]);
        let _ = queue.push_line("x");
        let shared = Rc::new(RefCell::new(Shared::default()));
        queue.attach(FakePipe(Rc::clone(&shared)));
        assert!(shared.borrow().dropped || written(&shared).is_empty());
        assert_eq!(queue.push_line("y"), Err(SendError::Closed));
    }
}

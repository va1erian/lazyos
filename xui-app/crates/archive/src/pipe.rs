//! A bounded in-process pipe between two threads.
//!
//! `lzma-rs` and `ruzstd`'s encoder push their output into a `Write` (and the
//! encoder pulls from a `Read`), while the tar parser pulls from a `Read`.
//! A pipe turns one shape into the other: the push side runs on its own
//! thread and blocks when [`DEPTH`] chunks are waiting, so memory stays
//! bounded whatever the stream's size. Dropping the reader makes the
//! writer's next call fail, so an abandoned stream never leaves a thread
//! blocked. The producer ends the stream with [`PipeWriter::finish`]; a writer
//! dropped without it (its thread died) is an error at the reader, never a
//! silently truncated stream.

use std::io::{self, Read, Write};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

/// Chunks in flight between the two ends.
const DEPTH: usize = 8;
/// Largest chunk a write sends.
const CHUNK: usize = 64 * 1024;

/// The message a pipe end carries: data, or the producer's error.
type Item = Result<Vec<u8>, io::Error>;

/// The writing end.
pub struct PipeWriter {
    tx: SyncSender<Item>,
}

/// The reading end.
pub struct PipeReader {
    rx: Receiver<Item>,
    chunk: Vec<u8>,
    pos: usize,
    done: bool,
}

/// A connected pair.
pub fn pipe() -> (PipeWriter, PipeReader) {
    let (tx, rx) = sync_channel(DEPTH);
    (
        PipeWriter { tx },
        PipeReader {
            rx,
            chunk: Vec::new(),
            pos: 0,
            done: false,
        },
    )
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the reading side went away")
}

impl PipeWriter {
    /// End the stream normally: the reader sees end of file.
    pub fn finish(self) {
        let _ = self.tx.send(Ok(Vec::new()));
    }

    /// End the stream with `error`: the reader's next read fails with it.
    pub fn fail(self, error: io::Error) {
        let _ = self.tx.send(Err(error));
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(CHUNK);
        if n == 0 {
            return Ok(0);
        }
        self.tx.send(Ok(buf[..n].to_vec())).map_err(|_| closed())?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.chunk.len() {
            if self.done {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(chunk)) if chunk.is_empty() => {
                    self.done = true;
                    return Ok(0);
                }
                Ok(Ok(chunk)) => {
                    self.chunk = chunk;
                    self.pos = 0;
                }
                Ok(Err(error)) => {
                    self.done = true;
                    return Err(error);
                }
                // The producer went away without finishing.
                Err(_) => {
                    self.done = true;
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the stream ended early",
                    ));
                }
            }
        }
        let n = buf.len().min(self.chunk.len() - self.pos);
        buf[..n].copy_from_slice(&self.chunk[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Wraps a reader or writer so a third-party encoder that `unwrap`s every
/// I/O result (`ruzstd`'s) never panics: the first error is kept, reads then
/// report end of stream and writes swallow their bytes, and the caller checks
/// [`Trap::take`] once the encoder returns.
pub struct Trap<T> {
    inner: T,
    error: Arc<Mutex<Option<io::Error>>>,
}

/// The error slot a [`Trap`] fills.
#[derive(Clone, Default)]
pub struct TrapSlot(Arc<Mutex<Option<io::Error>>>);

impl TrapSlot {
    /// The first error the trap caught, if any.
    pub fn take(&self) -> Option<io::Error> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl<T> Trap<T> {
    /// Wrap `inner`, reporting into `slot`.
    pub fn new(inner: T, slot: &TrapSlot) -> Trap<T> {
        Trap {
            inner,
            error: Arc::clone(&slot.0),
        }
    }

    fn failed(&self) -> bool {
        self.error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn keep(&self, error: io::Error) {
        let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(error);
    }

    /// The wrapped value.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<R: Read> Read for Trap<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failed() {
            return Ok(0);
        }
        loop {
            match self.inner.read(buf) {
                Ok(n) => return Ok(n),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.keep(error);
                    return Ok(0);
                }
            }
        }
    }
}

impl<W: Write> Write for Trap<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.failed() {
            if let Err(error) = self.inner.write_all(buf) {
                self.keep(error);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.failed() {
            if let Err(error) = self.inner.flush() {
                self.keep(error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_carries_a_large_stream_across_threads() {
        let (mut writer, mut reader) = pipe();
        let producer = std::thread::spawn(move || {
            for i in 0..1000u32 {
                writer.write_all(&[(i % 251) as u8; 1000]).unwrap();
            }
            writer.finish();
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        producer.join().unwrap();
        assert_eq!(out.len(), 1_000_000);
        assert_eq!(out[999_999], (999 % 251) as u8);
    }

    #[test]
    fn a_producer_that_dies_is_an_error_not_an_end() {
        let (writer, mut reader) = pipe();
        drop(writer);
        assert_eq!(
            reader.read(&mut [0; 4]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn a_dropped_reader_fails_the_writer() {
        let (mut writer, reader) = pipe();
        drop(reader);
        assert!(writer.write(b"x").is_err());
    }

    #[test]
    fn a_failed_producer_reaches_the_reader() {
        let (writer, mut reader) = pipe();
        writer.fail(io::Error::new(io::ErrorKind::InvalidData, "bad"));
        assert_eq!(
            reader.read(&mut [0; 4]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_trap_keeps_the_first_error() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let slot = TrapSlot::default();
        let mut trap = Trap::new(Failing, &slot);
        assert_eq!(trap.write(b"abc").unwrap(), 3);
        assert_eq!(slot.take().unwrap().to_string(), "disk full");
    }
}

//! Stream compression: the decoders a tarball or single file is read
//! through, and the encoders one is written through.
//!
//! Everything is a stream, so a multi-gigabyte `.tar.zst` never sits in
//! memory. The two push-style libraries run on a helper thread behind a
//! [`pipe`](crate::pipe): `lzma-rs` decodes xz into a pipe the caller reads,
//! and `ruzstd`'s encoder reads the caller's pipe; its `unwrap`s are kept
//! from panicking by [`Trap`].

use std::io::{self, BufRead, BufReader, Read, Write};
use std::thread;

use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::error::{Error, Result};
use crate::format::Level;
use crate::pipe::{pipe, PipeWriter, Trap, TrapSlot};

/// A readable stream that can cross to a helper thread.
pub type BoxRead = Box<dyn Read + Send>;
/// A writable stream that can cross to a helper thread.
pub type BoxWrite = Box<dyn Write + Send>;

/// A compression wrapped around a tarball or a single file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Uncompressed.
    None,
    /// gzip (RFC 1952), possibly several members.
    Gzip,
    /// xz (read-only).
    Xz,
    /// Zstandard, possibly several frames.
    Zstd,
}

impl Codec {
    /// The method name shown for a single compressed file.
    pub fn method(self) -> &'static str {
        match self {
            Codec::None => "Store",
            Codec::Gzip => "Deflate",
            Codec::Xz => "LZMA2",
            Codec::Zstd => "Zstandard",
        }
    }
}

/// `input` decompressed by `codec`.
pub fn decoder(codec: Codec, input: BoxRead) -> Result<BoxRead> {
    Ok(match codec {
        Codec::None => input,
        Codec::Gzip => Box::new(MultiGzDecoder::new(BufReader::new(input))),
        Codec::Xz => xz_decoder(input),
        Codec::Zstd => Box::new(ZstdFrames::new(input)),
    })
}

/// Decode xz on a helper thread into a pipe.
fn xz_decoder(input: BoxRead) -> BoxRead {
    let (writer, reader) = pipe();
    let spawned = thread::Builder::new()
        .name("xz-decode".into())
        .spawn(move || run_xz(input, writer));
    match spawned {
        Ok(_) => Box::new(reader),
        Err(error) => Box::new(FailedRead(Some(error))),
    }
}

fn run_xz(input: BoxRead, mut writer: PipeWriter) {
    let mut input = BufReader::new(input);
    match lzma_rs::xz_decompress(&mut input, &mut writer) {
        Ok(()) => writer.finish(),
        Err(error) => writer.fail(lzma_error(error)),
    }
}

/// An `lzma-rs` error as an `io::Error`: its own I/O failures keep their
/// kind (a cancellation travels through), the rest are damaged data.
pub(crate) fn lzma_error(error: lzma_rs::error::Error) -> io::Error {
    match error {
        lzma_rs::error::Error::IoError(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, format!("{other:?}")),
    }
}

/// A reader that fails once with the error that kept it from starting.
struct FailedRead(Option<io::Error>);

impl Read for FailedRead {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(self
            .0
            .take()
            .unwrap_or_else(|| io::Error::other("no decoder thread")))
    }
}

type ZstdDecoder =
    ruzstd::decoding::StreamingDecoder<BufReader<BoxRead>, ruzstd::decoding::FrameDecoder>;

/// Zstandard frames back to back (as `zstd` writes a concatenation).
struct ZstdFrames {
    state: ZstdState,
}

enum ZstdState {
    /// Before the next frame, holding the input.
    Between(BufReader<BoxRead>),
    /// Inside a frame.
    Frame(Box<ZstdDecoder>),
    /// The input ended or failed.
    Done,
}

impl ZstdFrames {
    fn new(input: BoxRead) -> ZstdFrames {
        ZstdFrames {
            state: ZstdState::Between(BufReader::new(input)),
        }
    }
}

impl Read for ZstdFrames {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match std::mem::replace(&mut self.state, ZstdState::Done) {
                ZstdState::Done => return Ok(0),
                ZstdState::Between(mut input) => {
                    if input.fill_buf()?.is_empty() {
                        return Ok(0);
                    }
                    let decoder = ruzstd::decoding::StreamingDecoder::new(input).map_err(|e| {
                        io::Error::new(io::ErrorKind::InvalidData, format!("zstd: {e}"))
                    })?;
                    self.state = ZstdState::Frame(Box::new(decoder));
                }
                ZstdState::Frame(mut decoder) => {
                    let n = decoder.read(buf)?;
                    if n > 0 {
                        self.state = ZstdState::Frame(decoder);
                        return Ok(n);
                    }
                    self.state = ZstdState::Between(decoder.into_inner());
                }
            }
        }
    }
}

/// A compressing writer that must be finished to complete its stream.
pub trait Finish: Write + Send {
    /// Flush the compressor's tail and the underlying writer.
    fn finish(self: Box<Self>) -> io::Result<()>;
}

/// `out` compressed by `codec` at `level`.
pub fn encoder(codec: Codec, level: Level, out: BoxWrite) -> Result<Box<dyn Finish>> {
    match codec {
        Codec::None => Ok(Box::new(Plain(out))),
        Codec::Gzip => Ok(Box::new(Gzip(GzEncoder::new(
            out,
            Compression::new(level.deflate()),
        )))),
        Codec::Zstd => zstd_encoder(level, out),
        Codec::Xz => Err(Error::unsupported("writing xz")),
    }
}

struct Plain(BoxWrite);

impl Write for Plain {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Finish for Plain {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.0.flush()
    }
}

struct Gzip(GzEncoder<BoxWrite>);

impl Write for Gzip {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Finish for Gzip {
    fn finish(self: Box<Self>) -> io::Result<()> {
        self.0.finish()?.flush()
    }
}

/// Zstandard encoding on a helper thread: the caller writes into a pipe the
/// encoder reads.
struct Zstd {
    writer: Option<PipeWriter>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}

fn zstd_encoder(level: Level, out: BoxWrite) -> Result<Box<dyn Finish>> {
    let level = match level {
        Level::Store => ruzstd::encoding::CompressionLevel::Uncompressed,
        // `ruzstd` implements only its fastest level; the others would panic.
        _ => ruzstd::encoding::CompressionLevel::Fastest,
    };
    let (writer, reader) = pipe();
    let thread = thread::Builder::new()
        .name("zstd-encode".into())
        .spawn(move || {
            let source_slot = TrapSlot::default();
            let drain_slot = TrapSlot::default();
            let source = Trap::new(reader, &source_slot);
            let mut drain = Trap::new(out, &drain_slot);
            ruzstd::encoding::compress(source, &mut drain, level);
            if let Some(error) = source_slot.take().or_else(|| drain_slot.take()) {
                return Err(error);
            }
            drain.into_inner().flush()
        })
        .map_err(Error::Io)?;
    Ok(Box::new(Zstd {
        writer: Some(writer),
        thread: Some(thread),
    }))
}

impl Write for Zstd {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.writer.as_mut() {
            Some(writer) => writer.write(buf),
            None => Err(io::Error::other("the zstd stream is finished")),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Finish for Zstd {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        if let Some(writer) = self.writer.take() {
            writer.finish();
        }
        match self.thread.take().map(thread::JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(io::Error::other("the zstd encoder failed")),
            None => Ok(()),
        }
    }
}

impl Drop for Zstd {
    fn drop(&mut self) {
        // An abandoned encoder: end its input so the thread stops.
        if let Some(writer) = self.writer.take() {
            writer.fail(io::Error::other("abandoned"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A `Write` into a shared buffer, so an encoder's output can be read
    /// back after `finish` consumed the writer.
    #[derive(Clone, Default)]
    pub(crate) struct Shared(pub Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn round_trip(codec: Codec, level: Level, data: &[u8]) -> Vec<u8> {
        let shared = Shared::default();
        let mut encoder = encoder(codec, level, Box::new(shared.clone())).unwrap();
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap();
        let compressed = shared.0.lock().unwrap().clone();
        let mut out = Vec::new();
        decoder(codec, Box::new(io::Cursor::new(compressed)))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn sample() -> Vec<u8> {
        (0..300_000u32).map(|i| (i * 7 % 13) as u8).collect()
    }

    #[test]
    fn gzip_round_trips() {
        assert_eq!(round_trip(Codec::Gzip, Level::Normal, &sample()), sample());
    }

    #[test]
    fn zstd_round_trips_at_both_levels() {
        assert_eq!(round_trip(Codec::Zstd, Level::Normal, &sample()), sample());
        assert_eq!(round_trip(Codec::Zstd, Level::Store, b"tiny"), b"tiny");
    }

    #[test]
    fn concatenated_zstd_frames_decode_as_one_stream() {
        let a = ruzstd::encoding::compress_to_vec(
            &b"hello "[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let b = ruzstd::encoding::compress_to_vec(
            &b"world"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let mut out = String::new();
        decoder(Codec::Zstd, Box::new(io::Cursor::new([a, b].concat())))
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn xz_decodes_through_the_pipe() {
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut &sample()[..], &mut compressed).unwrap();
        let mut out = Vec::new();
        decoder(Codec::Xz, Box::new(io::Cursor::new(compressed)))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, sample());
    }

    #[test]
    fn damaged_xz_is_an_error() {
        // Made by `xz` (CRC64 check); lzma-rs's own encoder writes no check.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pax.tar.xz");
        let mut compressed = std::fs::read(path).unwrap();
        let len = compressed.len();
        compressed[len / 2] ^= 0xff;
        let mut out = Vec::new();
        let result = decoder(Codec::Xz, Box::new(io::Cursor::new(compressed)))
            .unwrap()
            .read_to_end(&mut out);
        assert!(result.is_err());
    }

    #[test]
    fn xz_is_never_written() {
        assert!(encoder(Codec::Xz, Level::Normal, Box::new(io::sink())).is_err());
    }
}

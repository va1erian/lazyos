//! `lazyvideo`: what the Video Player needs to turn a file into frames on
//! time (docs/video-player-plan.md).
//!
//! * [`avi`]: the RIFF/AVI demuxer. It indexes the video stream by scanning
//!   chunk headers, so a file is read on demand, never whole.
//! * [`mjpeg`]: the Motion JPEG decoder (`zune-jpeg`).
//! * [`clock`]: the media clock and the show/wait/drop decision, pure so it
//!   is tested without sleeping.
//! * [`player`]: one thread that owns the file, paces frames and hands the
//!   newest to the window.
//!
//! New codecs and containers plug in behind [`Demuxer`] and [`Decoder`];
//! [`open`] picks them from the file's first bytes and codec tag.

#![forbid(unsafe_code)]

pub mod avi;
pub mod clock;
pub mod mjpeg;
pub mod player;

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

pub use player::{Frame, Player, Status};

/// Why a file did not open or a frame did not decode.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Not a container this player reads.
    NotRecognized,
    /// A container that contradicts itself; the text says where.
    Corrupt(&'static str),
    /// A codec (a four-character tag) the player has no decoder for.
    Unsupported(String),
    /// A frame the decoder refused.
    Decode(String),
    /// A declared size beyond what the player accepts.
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::NotRecognized => f.write_str("not a supported video container"),
            Error::Corrupt(what) => write!(f, "damaged file ({what})"),
            Error::Unsupported(codec) => write!(f, "unsupported video codec {codec}"),
            Error::Decode(why) => write!(f, "frame did not decode: {why}"),
            Error::TooLarge => f.write_str("the video is larger than the player accepts"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// The largest picture accepted: a frame is `w * h * 4` bytes in memory.
pub const MAX_PIXELS: u64 = 8192 * 4608;

/// What a file's video stream says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Frame rate as a fraction (`30000/1001`).
    pub fps_num: u32,
    pub fps_den: u32,
    /// The codec's four-character tag.
    pub codec: [u8; 4],
    pub frames: u64,
    /// The file carries an audio track (not played yet).
    pub has_audio: bool,
}

impl VideoInfo {
    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den.max(1) as f64
    }

    pub fn duration_us(&self) -> u64 {
        pts_us(self.frames, self.fps_num, self.fps_den)
    }

    /// The time one frame lasts.
    pub fn frame_us(&self) -> u64 {
        pts_us(1, self.fps_num, self.fps_den)
    }

    pub fn codec_name(&self) -> String {
        String::from_utf8_lossy(&self.codec).trim().to_owned()
    }
}

/// Presentation time of frame `index` at `fps_num / fps_den` frames a second.
pub fn pts_us(index: u64, fps_num: u32, fps_den: u32) -> u64 {
    let num = fps_num.max(1) as u128;
    (index as u128 * 1_000_000 * fps_den as u128 / num) as u64
}

/// The frame showing at `us`, for a stream of `frames` frames.
pub fn frame_at(us: u64, fps_num: u32, fps_den: u32, frames: usize) -> usize {
    let den = fps_den.max(1) as u128;
    let index = (us as u128 * fps_num as u128 / (1_000_000 * den)) as usize;
    index.min(frames.saturating_sub(1))
}

/// A container's video stream, read on demand.
pub trait Demuxer: Send {
    fn info(&self) -> &VideoInfo;
    /// Frames (packets) in the stream.
    fn frames(&self) -> usize;
    /// Whether every packet decodes alone, so playback may jump forward to
    /// catch up with the clock.
    fn all_key(&self) -> bool;
    /// The packet `index`, replacing the contents of `into`.
    fn read(&mut self, index: usize, into: &mut Vec<u8>) -> Result<(), Error>;
}

/// A codec: one packet in, one RGBA picture out.
pub trait Decoder: Send {
    /// Decodes `packet` into `rgba` (cleared and resized as needed) and
    /// returns the picture's width and height.
    fn decode(&mut self, packet: &[u8], rgba: &mut Vec<u8>) -> Result<(u32, u32), Error>;
}

/// An opened file: its demuxer and the decoder for its codec.
pub struct Media {
    pub info: VideoInfo,
    pub(crate) demux: Box<dyn Demuxer>,
    pub(crate) decoder: Box<dyn Decoder>,
}

impl Media {
    /// The demuxer and decoder, for a tool that drives them itself.
    pub fn into_parts(self) -> (Box<dyn Demuxer>, Box<dyn Decoder>) {
        (self.demux, self.decoder)
    }
}

/// Opens the video file at `path`.
pub fn open(path: &Path) -> Result<Media, Error> {
    open_reader(File::open(path)?)
}

/// Opens a video from any seekable reader.
pub fn open_reader<R: Read + Seek + Send + 'static>(reader: R) -> Result<Media, Error> {
    let demux = avi::Avi::open(reader)?;
    let info = demux.info().clone();
    let decoder: Box<dyn Decoder> = match &info.codec {
        b"MJPG" | b"mjpg" | b"AVI1" | b"jpeg" => Box::new(mjpeg::Mjpeg::new()),
        _ => return Err(Error::Unsupported(info.codec_name())),
    };
    Ok(Media {
        info,
        demux: Box::new(demux),
        decoder,
    })
}

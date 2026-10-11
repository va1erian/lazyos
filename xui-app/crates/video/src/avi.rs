//! The AVI (RIFF) demuxer.
//!
//! The file is scanned once, header by header, into an index of the video
//! stream's chunks (offset and length); a frame is read when it is wanted.
//! The scan never trusts a size: every chunk is clamped to its parent and to
//! the file, nesting is bounded, and a chunk that runs past the end of the
//! file (a download cut short) is dropped, not read.
//!
//! Not read: OpenDML super indexes (`AVIX` extension RIFFs are walked like
//! the first, so files over 1 GiB play), `idx1` (the scan replaces it, and
//! every Motion JPEG frame is a key frame), and audio (noted in
//! [`VideoInfo::has_audio`]).

use std::io::{Read, Seek, SeekFrom};

use crate::{Demuxer, Error, VideoInfo, MAX_PIXELS};

/// The most nested LISTs followed.
const MAX_DEPTH: u32 = 6;
/// The most video chunks indexed (a day of 60 fps video).
const MAX_CHUNKS: usize = 5_184_000;
/// The largest packet read into memory.
const MAX_PACKET: u32 = 64 << 20;

#[derive(Clone, Copy)]
struct Entry {
    offset: u64,
    len: u32,
}

/// One `strl`: what `strh` and `strf` said.
#[derive(Default)]
struct Stream {
    kind: [u8; 4],
    handler: [u8; 4],
    scale: u32,
    rate: u32,
    width: u32,
    height: u32,
    /// `biCompression` of a video `strf`, the tag a codec is named by.
    compression: [u8; 4],
}

#[derive(Default)]
struct Scan {
    streams: Vec<Stream>,
    /// `dwMicroSecPerFrame` of `avih`, the fallback frame rate.
    micros_per_frame: u32,
    video: Option<usize>,
    index: Vec<Entry>,
}

pub struct Avi<R> {
    reader: R,
    info: VideoInfo,
    index: Vec<Entry>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn tag_at(b: &[u8], at: usize) -> [u8; 4] {
    b.get(at..at + 4)
        .map_or([0; 4], |s| [s[0], s[1], s[2], s[3]])
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl<R: Read + Seek> Avi<R> {
    pub fn open(mut reader: R) -> Result<Avi<R>, Error> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; 12];
        if reader.read_exact(&mut head).is_err() || &head[..4] != b"RIFF" || &head[8..] != b"AVI " {
            return Err(Error::NotRecognized);
        }
        let mut scan = Scan::default();
        let mut pos = 0u64;
        // The first RIFF holds the headers; `AVIX` extensions follow it.
        while pos + 12 <= file_len {
            let mut riff = [0u8; 12];
            reader.seek(SeekFrom::Start(pos))?;
            if reader.read_exact(&mut riff).is_err() || &riff[..4] != b"RIFF" {
                break;
            }
            let size = u32_at(&riff, 4) as u64;
            let end = (pos + 8 + size).min(file_len);
            if &riff[8..] != b"AVI " && &riff[8..] != b"AVIX" {
                break;
            }
            walk(&mut reader, pos + 12, end, &mut scan, 0, false)?;
            pos = end + (end & 1);
        }
        let info = scan.info()?;
        let Scan { index, .. } = scan;
        Ok(Avi {
            reader,
            info,
            index,
        })
    }
}

impl Scan {
    fn info(&self) -> Result<VideoInfo, Error> {
        let n = self.video.ok_or(Error::Corrupt("no video stream"))?;
        let s = &self.streams[n];
        if s.width == 0 || s.height == 0 || s.width as u64 * s.height as u64 > MAX_PIXELS {
            return Err(Error::TooLarge);
        }
        if self.index.is_empty() {
            return Err(Error::Corrupt("no video frames"));
        }
        let (mut num, mut den) = (s.rate as u64, s.scale as u64);
        if num == 0 || den == 0 {
            num = 1_000_000;
            den = self.micros_per_frame as u64;
        }
        if num == 0 || den == 0 {
            return Err(Error::Corrupt("no frame rate"));
        }
        let g = gcd(num, den);
        let (num, den) = ((num / g) as u32, (den / g) as u32);
        // A tag in `strf` wins; the handler is what old writers filled in.
        let codec = if s.compression != [0; 4] {
            s.compression
        } else {
            s.handler
        };
        Ok(VideoInfo {
            width: s.width,
            height: s.height,
            fps_num: num,
            fps_den: den,
            codec,
            frames: self.index.len() as u64,
            has_audio: self.streams.iter().any(|s| &s.kind == b"auds"),
        })
    }
}

/// Walks the chunks of `pos..end`, descending into the LISTs that matter.
fn walk<R: Read + Seek>(
    r: &mut R,
    mut pos: u64,
    end: u64,
    scan: &mut Scan,
    depth: u32,
    in_movi: bool,
) -> Result<(), Error> {
    if depth > MAX_DEPTH {
        return Err(Error::Corrupt("lists nested too deep"));
    }
    while pos + 8 <= end {
        let mut head = [0u8; 8];
        r.seek(SeekFrom::Start(pos))?;
        if r.read_exact(&mut head).is_err() {
            break;
        }
        let id = tag_at(&head, 0);
        let size = u32_at(&head, 4) as u64;
        let body = pos + 8;
        let next = (body + size + (size & 1)).min(end.max(body));
        let body_end = (body + size).min(end);
        let want = |n: u64| body_end.saturating_sub(body).min(n) as usize;
        match &id {
            b"LIST" if size >= 4 => {
                let mut form = [0u8; 4];
                r.seek(SeekFrom::Start(body))?;
                r.read_exact(&mut form)?;
                match &form {
                    b"hdrl" | b"strl" | b"rec " => {
                        walk(r, body + 4, body_end, scan, depth + 1, in_movi)?
                    }
                    b"movi" => walk(r, body + 4, body_end, scan, depth + 1, true)?,
                    _ => {}
                }
            }
            b"avih" if !in_movi => {
                let mut b = vec![0u8; want(56)];
                r.seek(SeekFrom::Start(body))?;
                r.read_exact(&mut b)?;
                scan.micros_per_frame = u32_at(&b, 0);
            }
            b"strh" if !in_movi => {
                let mut b = vec![0u8; want(56)];
                r.seek(SeekFrom::Start(body))?;
                r.read_exact(&mut b)?;
                scan.streams.push(Stream {
                    kind: tag_at(&b, 0),
                    handler: tag_at(&b, 4),
                    scale: u32_at(&b, 20),
                    rate: u32_at(&b, 24),
                    ..Stream::default()
                });
                if scan.video.is_none()
                    && &scan.streams.last().map_or([0; 4], |s| s.kind) == b"vids"
                {
                    scan.video = Some(scan.streams.len() - 1);
                }
            }
            b"strf" if !in_movi => {
                let mut b = vec![0u8; want(40)];
                r.seek(SeekFrom::Start(body))?;
                r.read_exact(&mut b)?;
                if let Some(s) = scan.streams.last_mut() {
                    if &s.kind == b"vids" {
                        // BITMAPINFOHEADER; a bottom-up bitmap has a
                        // negative height.
                        s.width = u32_at(&b, 4);
                        s.height = (u32_at(&b, 8) as i32).unsigned_abs();
                        s.compression = tag_at(&b, 16);
                    }
                }
            }
            _ if in_movi => {
                // `NNdc` / `NNdb`: stream number in two decimal digits,
                // then a compressed or uncompressed video frame.
                let digits = id[0].is_ascii_digit() && id[1].is_ascii_digit();
                let number =
                    (id[0].wrapping_sub(b'0') as usize) * 10 + id[1].wrapping_sub(b'0') as usize;
                let video = (&id[2..] == b"dc" || &id[2..] == b"db")
                    && digits
                    && scan.video == Some(number);
                // A frame cut off by the end of the file is not a frame.
                if video && body + size <= end && size > 0 {
                    if scan.index.len() >= MAX_CHUNKS {
                        return Err(Error::TooLarge);
                    }
                    scan.index.push(Entry {
                        offset: body,
                        len: size as u32,
                    });
                }
            }
            _ => {}
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    Ok(())
}

impl<R: Read + Seek + Send> Demuxer for Avi<R> {
    fn info(&self) -> &VideoInfo {
        &self.info
    }

    fn frames(&self) -> usize {
        self.index.len()
    }

    fn all_key(&self) -> bool {
        // Only Motion JPEG is decoded so far, and it has no other frames.
        true
    }

    fn read(&mut self, index: usize, into: &mut Vec<u8>) -> Result<(), Error> {
        let entry = *self.index.get(index).ok_or(Error::Corrupt("frame index"))?;
        if entry.len > MAX_PACKET {
            return Err(Error::TooLarge);
        }
        into.clear();
        into.resize(entry.len as usize, 0);
        self.reader.seek(SeekFrom::Start(entry.offset))?;
        self.reader.read_exact(into)?;
        Ok(())
    }
}

/// Builds a minimal AVI around `frames`, for tests and fixtures: one video
/// stream tagged `codec`, `fps` frames a second, no index.
pub fn build_avi(
    codec: &[u8; 4],
    width: u32,
    height: u32,
    fps: u32,
    frames: &[Vec<u8>],
) -> Vec<u8> {
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }
    fn list(form: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
        let mut body = form.to_vec();
        parts.iter().for_each(|p| body.extend(p));
        chunk(b"LIST", &body)
    }
    let le = |v: u32| v.to_le_bytes();
    let mut avih = Vec::new();
    for v in [
        1_000_000 / fps.max(1),
        0,
        0,
        0,
        frames.len() as u32,
        0,
        1,
        0,
        width,
        height,
    ] {
        avih.extend(le(v));
    }
    avih.resize(56, 0);
    let mut strh = b"vids".to_vec();
    strh.extend(codec);
    for v in [0, 0, 0, 1, fps, 0, frames.len() as u32] {
        strh.extend(le(v));
    }
    strh.resize(56, 0);
    let mut strf = Vec::new();
    for v in [40, width, height] {
        strf.extend(le(v));
    }
    strf.extend([1, 0, 24, 0]);
    strf.extend(codec);
    strf.resize(40, 0);
    let hdrl = list(
        b"hdrl",
        &[
            chunk(b"avih", &avih),
            list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)]),
        ],
    );
    let movi = list(
        b"movi",
        &frames.iter().map(|f| chunk(b"00dc", f)).collect::<Vec<_>>(),
    );
    let mut body = b"AVI ".to_vec();
    body.extend(hdrl);
    body.extend(movi);
    chunk(b"RIFF", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn open(bytes: Vec<u8>) -> Result<Avi<Cursor<Vec<u8>>>, Error> {
        Avi::open(Cursor::new(bytes))
    }

    fn frames(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| vec![i as u8; 5 + i]).collect()
    }

    #[test]
    fn indexes_every_frame_and_reads_each_back() {
        let mut avi = open(build_avi(b"MJPG", 64, 48, 25, &frames(7))).unwrap();
        let info = avi.info().clone();
        assert_eq!((info.width, info.height), (64, 48));
        assert_eq!((info.fps_num, info.fps_den, info.frames), (25, 1, 7));
        assert_eq!(&info.codec, b"MJPG");
        let mut buf = Vec::new();
        for i in 0..7 {
            avi.read(i, &mut buf).unwrap();
            assert_eq!(buf, vec![i as u8; 5 + i]);
        }
        assert!(avi.read(7, &mut buf).is_err());
    }

    #[test]
    fn a_frame_rate_is_reduced_to_lowest_terms() {
        let avi = open(build_avi(b"MJPG", 8, 8, 50, &frames(2))).unwrap();
        // 50/1 stays; the duration is the frames over the rate.
        assert_eq!(avi.info().duration_us(), 40_000);
    }

    #[test]
    fn a_file_that_is_not_avi_is_refused() {
        assert!(matches!(open(vec![]), Err(Error::NotRecognized)));
        assert!(matches!(
            open(b"RIFF\0\0\0\0WAVEfmt ".to_vec()),
            Err(Error::NotRecognized)
        ));
        assert!(matches!(
            open(b"\x00\x00\x00\x18ftypmp42".to_vec()),
            Err(Error::NotRecognized)
        ));
    }

    #[test]
    fn a_file_cut_short_keeps_the_frames_it_has() {
        let full = build_avi(b"MJPG", 16, 16, 10, &frames(6));
        // Cut in the middle of the fifth frame's data.
        let cut = full.len() - 24;
        let avi = open(full[..cut].to_vec()).unwrap();
        assert!(avi.frames() >= 3 && avi.frames() < 6, "{}", avi.frames());
    }

    #[test]
    fn a_chunk_that_claims_more_than_the_file_is_not_indexed() {
        let mut bytes = build_avi(b"MJPG", 16, 16, 10, &frames(2));
        // Grow the last chunk's declared size far past the end.
        let at = bytes.len() - (5 + 1 + 6 + 1) - 4;
        bytes[at..at + 4].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
        let avi = open(bytes).unwrap();
        assert_eq!(avi.frames(), 1);
    }

    #[test]
    fn no_frames_or_no_size_is_an_error_not_a_panic() {
        assert!(open(build_avi(b"MJPG", 16, 16, 10, &[])).is_err());
        assert!(open(build_avi(b"MJPG", 0, 16, 10, &frames(1))).is_err());
        assert!(matches!(
            open(build_avi(b"MJPG", 70_000, 70_000, 10, &frames(1))),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn every_truncation_and_bit_flip_returns_without_panicking() {
        let good = build_avi(b"MJPG", 32, 24, 30, &frames(5));
        for cut in 0..good.len() {
            let _ = open(good[..cut].to_vec());
        }
        let mut seed = 0x2545_f491u32;
        for _ in 0..4000 {
            let mut bytes = good.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed >> 8) as usize % bytes.len();
                bytes[at] ^= (seed >> 3) as u8 | 1;
            }
            if let Ok(mut avi) = open(bytes) {
                let mut buf = Vec::new();
                for i in 0..avi.frames().min(8) {
                    let _ = avi.read(i, &mut buf);
                }
            }
        }
    }
}

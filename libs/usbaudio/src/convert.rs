//! The mixer's interleaved stereo `S16Le` as the device's samples: 1 to 8
//! channels, each in a 2-, 3- or 4-byte little-endian subslot with the
//! sample in its most significant bytes (UAC Frmts 2.3.1.1, 2.3.1.3).
//!
//! One channel gets the average of left and right; more than two get left
//! and right in the first two and silence in the rest (a 5.1 or 7.1 device
//! orders front left, front right first).

use crate::desc::Format;

/// Bytes of one source frame (stereo `S16Le`).
pub const SOURCE_FRAME: usize = 4;

/// Write `frames` source frames from `source` into `out` in `format`;
/// returns the bytes written. Stops early at whichever buffer ends first.
pub fn convert(source: &[u8], format: &Format, out: &mut [u8]) -> usize {
    let frame = format.frame_bytes() as usize;
    let mut written = 0;
    let (frames, _) = source.as_chunks::<SOURCE_FRAME>();
    for (src, dst) in frames.iter().zip(out.chunks_exact_mut(frame)) {
        let left = i16::from_le_bytes([src[0], src[1]]);
        let right = i16::from_le_bytes([src[2], src[3]]);
        let subslot = usize::from(format.subslot);
        for (channel, sample) in dst.chunks_exact_mut(subslot).enumerate() {
            let value = match (format.channels, channel) {
                (1, _) => ((i32::from(left) + i32::from(right)) / 2) as i16,
                (_, 0) => left,
                (_, 1) => right,
                _ => 0,
            };
            put(sample, value);
        }
        written += frame;
    }
    written
}

/// Silence for as many whole frames of `format` as fit in `out`; returns
/// the bytes written.
pub fn silence(format: &Format, out: &mut [u8]) -> usize {
    let frame = format.frame_bytes() as usize;
    let len = out.len() / frame * frame;
    out[..len].fill(0);
    len
}

/// One 16-bit sample in a 2-, 3- or 4-byte subslot, most significant bytes.
fn put(subslot: &mut [u8], value: i16) {
    let [lo, hi] = value.to_le_bytes();
    let n = subslot.len();
    subslot[..n - 2].fill(0);
    subslot[n - 2] = lo;
    subslot[n - 1] = hi;
}

//! Frames the headless check counts: FNV-1a over a frame's RGBA bytes, as
//! the engine's `web/bench.py` prints frame digests over the same pixels —
//! and `quaketool play` natively. The verdict line's `crc` field is exactly
//! this value, so a run reported on one machine is comparable on any other
//! (a fresh process boots the same game state: id's random stream is seeded
//! per process).

/// FNV-1a over the frame's bytes (the record's RGBA, a byte at a time).
pub fn frame(rgba: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in rgba {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_frame_is_the_initial_hash() {
        assert_eq!(frame(&[]), 0x811c_9dc5);
    }

    #[test]
    fn it_is_a_real_fnv_1a() {
        // The four bytes through the 0x01000193 prime, one at a time —
        // spelled out here so a change of formula cannot hide behind the
        // constant.
        let mut h = 0x811c_9dc5u32;
        for byte in [0u8, 0, 0, 255] {
            h = (h ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        }
        assert_eq!(frame(&[0, 0, 0, 255]), h);
    }

    #[test]
    fn order_matters() {
        assert_ne!(frame(&[0, 0, 0, 255]), frame(&[255, 0, 0, 0]));
    }
}

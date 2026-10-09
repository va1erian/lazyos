//! The records our platform side writes to the engine's input stream and
//! reads off its output stream ([`crate::proto`] in the assembled tree,
//! which documents the whole layout).
//!
//! Small enough that the platform side keeps its own encoders — but nothing
//! else in the tree may know these bytes: the tests at the bottom run our
//! encoders through the engine's real parser
//! ([`read_event`]), so the two halves cannot drift.

/// The record kinds our side writes.
pub const IN_TICK: u8 = 1;
pub const IN_KEY: u8 = 2;
pub const IN_CLEAR_KEYS: u8 = 4;
pub const IN_CALL: u8 = 7;
pub const IN_WINDOW: u8 = 9;
/// The record kinds our side reads (see [`crate::sink`]).
pub const OUT_FRAME: u8 = 1;
pub const OUT_SYNC: u8 = 2;
pub const OUT_STATE: u8 = 3;
pub const OUT_PCM: u8 = 14;
pub const OUT_QUIT: u8 = 18;

/// A `Tick` record: `seq u32`, `dt f64` — a display refresh with the
/// seconds since the last one (`proto.rs`: the page's `Tick`).
pub fn tick(seq: u32, dt: f64) -> Vec<u8> {
    let mut record = vec![IN_TICK, 0];
    record.extend_from_slice(&12u16.to_le_bytes());
    record.extend_from_slice(&seq.to_le_bytes());
    record.extend_from_slice(&dt.to_le_bytes());
    record
}

/// A `Key` record: `keynum u8`, `down u8`, `0 u16`, `ch u32`.
pub fn key(keynum: u8, down: bool, ch: u32) -> Vec<u8> {
    let mut record = vec![IN_KEY, 0];
    record.extend_from_slice(&8u16.to_le_bytes());
    record.push(keynum);
    record.push(u8::from(down));
    record.extend_from_slice(&0u16.to_le_bytes());
    record.extend_from_slice(&ch.to_le_bytes());
    record
}

/// A `ClearKeys` record: every held key is released (the window lost the
/// keyboard, `keys.c`'s `ClearAllStates`).
pub fn clear_keys() -> Vec<u8> {
    vec![IN_CLEAR_KEYS, 0, 0, 0]
}

/// A `Call` record: `id u32`, then a UTF-8 line (`automation.rs`'s calls).
pub fn call(id: u32, line: &str) -> Vec<u8> {
    let mut record = vec![IN_CALL, 0];
    let line = line.as_bytes();
    // The payload is the id (4 bytes) and the line; a record is at most
    // 65535 bytes, and `records` truncates a longer name's tail well
    // before that.
    let payload = (line.len() + 4) as u16;
    record.extend_from_slice(&payload.to_le_bytes());
    record.extend_from_slice(&id.to_le_bytes());
    record.extend_from_slice(line);
    record
}

/// A `Window` record: the picture's box in device pixels (`vid.rs`'s
/// `set_window`).
pub fn window(w: u32, h: u32) -> Vec<u8> {
    let mut record = vec![IN_WINDOW, 0];
    record.extend_from_slice(&8u16.to_le_bytes());
    record.extend_from_slice(&w.to_le_bytes());
    record.extend_from_slice(&h.to_le_bytes());
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed our records through the engine's own parser: it must decode
    /// exactly what the comment says. `crate::proto::read_event` exists
    /// here for these tests — the canonical reader tests the encoders keep
    /// honest.
    #[test]
    fn our_records_decode_the_way_the_engine_says() {
        use crate::proto::read_event;
        let mut stream = Vec::new();
        stream.extend(tick(7, std::f64::consts::FRAC_1_SQRT_2));
        stream.extend(key(b'w', true, b'w' as u32));
        stream.extend(clear_keys());
        stream.extend(call(9, "boot"));
        stream.extend(window(960, 720));
        let mut slice = &stream[..];
        let mut got = Vec::new();
        while let Some(event) = read_event(&mut slice).unwrap() {
            got.push(event);
        }
        assert_eq!(
            got,
            [
                crate::proto::Event::Tick { seq: 7, dt: std::f64::consts::FRAC_1_SQRT_2 },
                crate::proto::Event::Key { keynum: b'w', down: true, ch: b'w' as u32 },
                crate::proto::Event::ClearKeys,
                crate::proto::Event::Call { id: 9, line: "boot".into() },
                crate::proto::Event::Window { w: 960, h: 720 },
            ]
        );
    }
}

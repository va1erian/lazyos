#![forbid(unsafe_code)]

//! CRC-32 and a bounded DEFLATE decoder, for the zip container.
//!
//! The paint crate depends on `xui-core` only (it must build for a toy OS),
//! so it carries the ~150 lines it needs instead of `flate2`. Writing never
//! compresses: layer PNGs are already deflated. Reading must still accept
//! deflated entries, because GIMP and Krita compress `stack.xml`.

/// The CRC-32 (IEEE) lookup table.
const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// The CRC-32 of `data`, as zip stores it.
pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &b| {
        TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

const MAX_BITS: usize = 15;

/// A canonical Huffman code in the counting form of zlib's `puff`.
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// Builds a code from per-symbol bit lengths. An over-subscribed set is
    /// refused; an incomplete one is allowed (using a missing code fails).
    fn new(lengths: &[u8]) -> Result<Huffman, &'static str> {
        let mut count = [0u16; MAX_BITS + 1];
        for &len in lengths {
            count[usize::from(len)] += 1;
        }
        let mut left = 1i32;
        for &c in &count[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err("over-subscribed code");
            }
        }
        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offsets[len + 1] = offsets[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbol[usize::from(offsets[usize::from(len)])] = sym as u16;
                offsets[usize::from(len)] += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    bits: u32,
    held: u32,
}

impl Reader<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, &'static str> {
        while self.held < need {
            let byte = *self.data.get(self.pos).ok_or("truncated data")?;
            self.pos += 1;
            self.bits |= u32::from(byte) << self.held;
            self.held += 8;
        }
        let value = self.bits & ((1u32 << need) - 1);
        self.bits >>= need;
        self.held -= need;
        Ok(value)
    }

    fn symbol(&mut self, h: &Huffman) -> Result<usize, &'static str> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAX_BITS {
            code |= self.bits(1)? as i32;
            let count = i32::from(h.count[len]);
            if code - count < first {
                return h
                    .symbol
                    .get((index + (code - first)) as usize)
                    .map(|&s| usize::from(s))
                    .ok_or("bad code");
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("bad code")
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decodes a raw DEFLATE stream, refusing to produce more than `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut r = Reader {
        data,
        pos: 0,
        bits: 0,
        held: 0,
    };
    let mut out = Vec::new();
    loop {
        let last = r.bits(1)? == 1;
        match r.bits(2)? {
            0 => stored(&mut r, &mut out, limit)?,
            1 => {
                let mut lengths = [8u8; 288];
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                codes(&mut r, &mut out, limit, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut r)?;
                codes(&mut r, &mut out, limit, &lit, &dist)?;
            }
            _ => return Err("bad block type"),
        }
        if last {
            return Ok(out);
        }
    }
}

fn stored(r: &mut Reader, out: &mut Vec<u8>, limit: usize) -> Result<(), &'static str> {
    // Drop the rest of the current byte; the bit buffer holds whole bytes only.
    r.bits >>= r.held % 8;
    r.held -= r.held % 8;
    let len = r.bits(16)? as usize;
    let inverse = r.bits(16)? as usize;
    if len != !inverse & 0xFFFF {
        return Err("bad stored block");
    }
    if out.len() + len > limit {
        return Err("data too large");
    }
    // Whole bytes still in the bit buffer come first.
    let mut left = len;
    while left > 0 && r.held >= 8 {
        out.push(r.bits(8)? as u8);
        left -= 1;
    }
    let end = r.pos.checked_add(left).ok_or("truncated data")?;
    out.extend_from_slice(r.data.get(r.pos..end).ok_or("truncated data")?);
    r.pos = end;
    Ok(())
}

fn dynamic(r: &mut Reader) -> Result<(Huffman, Huffman), &'static str> {
    let nlen = r.bits(5)? as usize + 257;
    let ndist = r.bits(5)? as usize + 1;
    let ncode = r.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err("bad counts");
    }
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut cl = [0u8; 19];
    for &slot in ORDER.iter().take(ncode) {
        cl[slot] = r.bits(3)? as u8;
    }
    let code_code = Huffman::new(&cl)?;
    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = r.symbol(&code_code)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (value, repeat) = match sym {
            16 => (
                *lengths[..i].last().ok_or("no previous length")?,
                3 + r.bits(2)?,
            ),
            17 => (0, 3 + r.bits(3)?),
            _ => (0, 11 + r.bits(7)?),
        };
        let end = i + repeat as usize;
        if end > lengths.len() {
            return Err("too many lengths");
        }
        lengths[i..end].fill(value);
        i = end;
    }
    if lengths[256] == 0 {
        return Err("no end-of-block code");
    }
    Ok((
        Huffman::new(&lengths[..nlen])?,
        Huffman::new(&lengths[nlen..])?,
    ))
}

fn codes(
    r: &mut Reader,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), &'static str> {
    loop {
        let sym = r.symbol(lit)?;
        if sym < 256 {
            if out.len() >= limit {
                return Err("data too large");
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let sym = sym - 257;
        if sym >= 29 {
            return Err("bad length code");
        }
        let len = usize::from(LENGTH_BASE[sym]) + r.bits(u32::from(LENGTH_EXTRA[sym]))? as usize;
        let d = r.symbol(dist)?;
        if d >= 30 {
            return Err("bad distance code");
        }
        let back = usize::from(DIST_BASE[d]) + r.bits(u32::from(DIST_EXTRA[d]))? as usize;
        if back > out.len() {
            return Err("distance too far back");
        }
        if out.len() + len > limit {
            return Err("data too large");
        }
        // Byte by byte: a match may overlap its own output.
        for _ in 0..len {
            out.push(out[out.len() - back]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"image/openraster"), crc32(b"image/openraster"));
    }

    /// `zlib.compressobj(9, DEFLATED, -15)` of `b"hello hello hello hello\n"`:
    /// one fixed-Huffman block with a back-reference.
    const FIXED: [u8; 11] = [
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0xb9, 0x00,
    ];

    /// The same encoder over 300 skewed bytes (`random.seed(5)`, choices from
    /// `aaaaaaabbbbccde \n`): a dynamic-Huffman block.
    const DYNAMIC: [u8; 136] = [
        0x2d, 0x8f, 0xd1, 0x0d, 0xc0, 0x40, 0x08, 0x42, 0xff, 0x9d, 0xc2, 0xd5, 0x00, 0xdd, 0x7f,
        0x85, 0x82, 0xbd, 0x36, 0x4d, 0x14, 0xf1, 0x49, 0xa9, 0xc2, 0x12, 0x80, 0x9a, 0x82, 0x8b,
        0x21, 0xe4, 0x76, 0xf1, 0x1e, 0x66, 0x08, 0x59, 0x1d, 0x37, 0x69, 0xfd, 0x8a, 0x4d, 0x74,
        0x47, 0x90, 0x30, 0xfe, 0xce, 0xcf, 0x55, 0xa9, 0xb8, 0xb1, 0xc7, 0x5d, 0x3a, 0xff, 0x21,
        0xd8, 0xc1, 0x64, 0x75, 0x3d, 0x42, 0x47, 0x73, 0x31, 0xba, 0x93, 0x8b, 0xda, 0x36, 0x94,
        0xad, 0xb8, 0x32, 0x6d, 0x96, 0xf7, 0xf5, 0x1f, 0xe3, 0x09, 0xe4, 0x9a, 0x29, 0xca, 0x5b,
        0x2b, 0x16, 0xea, 0x72, 0x9b, 0x53, 0x73, 0x47, 0x16, 0x7c, 0x68, 0x17, 0x3e, 0x38, 0xeb,
        0x68, 0x26, 0x38, 0x25, 0x27, 0x2c, 0x67, 0xa2, 0xdb, 0x19, 0x6d, 0x56, 0x2f, 0xd2, 0xc9,
        0x9a, 0xf1, 0x99, 0x56, 0x12, 0xb7, 0xbd, 0xbc, 0x3f, 0x34, 0x31, 0xdc, 0x04, 0x62, 0xe3,
        0x03,
    ];

    #[test]
    fn a_fixed_huffman_block_decodes() {
        let out = inflate(&FIXED, 1000).unwrap();
        assert_eq!(out, b"hello hello hello hello\n".to_vec());
    }

    #[test]
    fn a_dynamic_huffman_block_decodes() {
        let out = inflate(&DYNAMIC, 1000).unwrap();
        assert_eq!(out.len(), 300);
        assert_eq!(crc32(&out), 0x5c42_b816);
        assert!(inflate(&DYNAMIC, 299).is_err(), "limit");
    }

    #[test]
    fn a_stored_block_decodes_and_the_limit_holds() {
        let mut data = vec![0x01, 5, 0, 0xFA, 0xFF];
        data.extend_from_slice(b"abcde");
        assert_eq!(inflate(&data, 5).unwrap(), b"abcde");
        assert!(inflate(&data, 4).is_err(), "limit");
        // A bad complement is refused.
        data[3] = 0;
        assert!(inflate(&data, 99).is_err());
    }

    #[test]
    fn hostile_streams_are_refused_not_panicked() {
        assert!(inflate(&[], 10).is_err());
        assert!(inflate(&[0x07], 10).is_err(), "block type 3");
        assert!(inflate(&[0xFF; 64], 1 << 20).is_err());
        // A back-reference before the start.
        assert!(inflate(&[0x03, 0x02], 10).is_err());
        // Any prefix of a valid stream is an error or a short success, never a panic.
        for n in 0..DYNAMIC.len() {
            let _ = inflate(&DYNAMIC[..n], 1000);
        }
        for n in 0..FIXED.len() {
            assert!(inflate(&FIXED[..n], 100).is_err(), "prefix {n}");
        }
        // A flipped bit anywhere is an error or other data, never a panic.
        for i in 0..DYNAMIC.len() * 8 {
            let mut data = DYNAMIC;
            data[i / 8] ^= 1 << (i % 8);
            let _ = inflate(&data, 1000);
        }
    }
}

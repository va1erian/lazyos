//! CRC-32 as GPT uses it: IEEE 802.3, reflected, initial and final value
//! `0xFFFF_FFFF` (the zlib/PNG CRC).

const POLY: u32 = 0xEDB8_8320;

const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            bit += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &byte| {
        TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

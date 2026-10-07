//! A minimal `.lzp` writer for the provisioning tests: real archives that
//! `lazypkg` opens, so provisioning runs the reader, the digest and the
//! extraction exactly as `pkgd` does.

use crc::{Crc, CRC_32_ISO_HDLC};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

/// The PNG signature plus a few bytes: what the reader checks of an icon.
fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    bytes
}

/// Pseudo-random bytes (xorshift), so a "program" does not compress away and
/// each build of a package differs.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// A core-style package: manifest, a program of `program_len` bytes made
/// from `seed`, the three icons, one page of documentation and an empty
/// resource.
pub fn package(system_name: &str, version: &str, program_len: usize, seed: u64) -> Vec<u8> {
    package_with(system_name, version, program_len, seed, "")
}

/// [`package`] with `extra` appended to the manifest right after `[entry]`'s
/// fields (more `[entry]` keys, then any further tables).
pub fn package_with(
    system_name: &str,
    version: &str,
    program_len: usize,
    seed: u64,
    extra: &str,
) -> Vec<u8> {
    let short = system_name.rsplit('.').next().unwrap();
    let manifest = format!(
        "[app]\nname = \"{short}\"\nsystem_name = \"{system_name}\"\nauthor = \"LazyOS\"\n\
         version = \"{version}\"\ncategory = \"utilities\"\n\n[entry]\nbinary = \"bin/{short}.elf\"\n\
         args = [\"--client\"]\nabi = \"linux\"\n{extra}"
    );
    let members: Vec<(String, Vec<u8>, bool)> = vec![
        ("manifest.toml".into(), manifest.into_bytes(), true),
        (format!("bin/{short}.elf"), noise(program_len, seed), true),
        ("icons/app-16.png".into(), png(), false),
        ("icons/app-32.png".into(), png(), false),
        ("icons/app-128.png".into(), png(), false),
        (
            "docs/README.md".into(),
            format!("# {short} {version} build {seed}\n").into_bytes(),
            true,
        ),
        // Unpacking an empty file yields no piece; it must still exist.
        ("resources/empty".into(), Vec::new(), true),
    ];
    zip(&members)
}

/// Serialize `(name, data, deflate)` members.
fn zip(members: &[(String, Vec<u8>, bool)]) -> Vec<u8> {
    let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC);
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in members {
        let offset = out.len() as u32;
        let sum = crc.checksum(data);
        let (method, body) = if *deflate {
            (8u16, miniz_oxide::deflate::compress_to_vec(data, 6))
        } else {
            (0u16, data.clone())
        };
        let header = |sig: u32, central_record: bool, out: &mut Vec<u8>| {
            put32(out, sig);
            if central_record {
                put16(out, 20);
            }
            for value in [20u16, 0, method, 0, 0] {
                put16(out, value);
            }
            put32(out, sum);
            put32(out, body.len() as u32);
            put32(out, data.len() as u32);
            put16(out, name.len() as u16);
            put16(out, 0);
            if central_record {
                for value in [0u16, 0, 0] {
                    put16(out, value);
                }
                put32(out, 0);
                put32(out, offset);
            }
            out.extend_from_slice(name.as_bytes());
        };
        header(LOCAL_SIG, false, &mut out);
        out.extend_from_slice(&body);
        header(CENTRAL_SIG, true, &mut central);
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    put32(&mut out, EOCD_SIG);
    for value in [0u16, 0, members.len() as u16, members.len() as u16] {
        put16(&mut out, value);
    }
    put32(&mut out, central.len() as u32);
    put32(&mut out, central_offset);
    put16(&mut out, 0);
    out
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

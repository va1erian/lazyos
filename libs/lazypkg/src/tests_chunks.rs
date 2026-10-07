//! [`crate::Package::read_chunks`]: the pieces add up to [`crate::Package::read`]
//! for entries much larger than the window, and every bad entry still fails.

use alloc::vec;
use alloc::vec::Vec;

use crate::testzip::{self, Member};
use crate::{ChunkError, Package, ReadError, CHUNK};

/// `len` bytes that compress, but only through back-references that reach
/// across the window's wrap (a 40 000-byte block of noise, repeated).
fn program(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let block: Vec<u8> = (0..40_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    block.iter().copied().cycle().take(len).collect()
}

/// A valid package whose program is `member`.
fn with_program(member: Member<'static>) -> Vec<u8> {
    let mut members = testzip::valid_members(false);
    members.retain(|m| m.name != "bin/app.elf");
    members.push(member);
    testzip::build(&members)
}

/// Every piece of `name`, or the error.
fn pieces(package: &Package<'_>, name: &str) -> Result<Vec<Vec<u8>>, ChunkError<()>> {
    let mut pieces = Vec::new();
    package.read_chunks(name, |piece| {
        pieces.push(piece.to_vec());
        Ok::<(), ()>(())
    })?;
    Ok(pieces)
}

#[test]
fn pieces_add_up_to_the_entry_stored_and_deflated() {
    let data = program(5 * CHUNK / 2 + 123);
    for member in [
        Member::stored("bin/app.elf", data.clone()),
        Member::deflated("bin/app.elf", data.clone()),
    ] {
        let bytes = with_program(member);
        let package = Package::open(&bytes).unwrap();
        let pieces = pieces(&package, "bin/app.elf").unwrap();
        assert!(pieces.len() >= 3, "{} pieces", pieces.len());
        assert!(pieces.iter().all(|p| !p.is_empty() && p.len() <= CHUNK));
        assert_eq!(pieces.concat(), data);
        assert_eq!(package.read("bin/app.elf").unwrap(), data);
    }
}

#[test]
fn an_empty_entry_emits_nothing() {
    let bytes = with_program(Member::deflated("bin/app.elf", Vec::new()));
    let package = Package::open(&bytes).unwrap();
    assert_eq!(
        pieces(&package, "bin/app.elf").unwrap(),
        Vec::<Vec<u8>>::new()
    );
}

#[test]
fn a_bad_crc_fails_after_the_last_piece() {
    let mut member = Member::deflated("bin/app.elf", program(CHUNK + 1));
    member.crc = Some(0x1234_5678);
    let bytes = with_program(member);
    let package = Package::open(&bytes).unwrap();
    assert!(matches!(
        pieces(&package, "bin/app.elf"),
        Err(ChunkError::Read(ReadError::CrcMismatch {
            expected: 0x1234_5678,
            ..
        }))
    ));
}

#[test]
fn nothing_past_the_declared_size_is_emitted() {
    // A stored entry whose sizes disagree is refused at open already.
    let data = program(3 * CHUNK);
    for declared in [CHUNK as u32 + 7, 3 * CHUNK as u32 + 1] {
        let member = Member::deflated("bin/app.elf", data.clone());
        let bytes = with_program(member.declared_size(declared));
        let package = Package::open(&bytes).unwrap();
        let mut emitted = 0usize;
        let result = package.read_chunks("bin/app.elf", |piece| {
            emitted += piece.len();
            Ok::<(), ()>(())
        });
        assert!(
            matches!(
                result,
                Err(ChunkError::Read(ReadError::SizeMismatch { .. }))
            ),
            "{result:?}"
        );
        assert!(emitted <= declared as usize, "{emitted} > {declared}");
    }
}

#[test]
fn a_corrupt_stream_fails() {
    let mut member = Member::stored("bin/app.elf", vec![0xff; 64]);
    member.method = Some(8);
    let bytes = with_program(member.declared_size(16));
    let package = Package::open(&bytes).unwrap();
    assert!(matches!(
        pieces(&package, "bin/app.elf"),
        Err(ChunkError::Read(ReadError::Corrupt { .. }))
    ));
}

#[test]
fn a_refusing_sink_stops_the_stream() {
    let bytes = with_program(Member::deflated("bin/app.elf", program(3 * CHUNK)));
    let package = Package::open(&bytes).unwrap();
    let mut calls = 0;
    let result = package.read_chunks("bin/app.elf", |_| {
        calls += 1;
        Err("disk full")
    });
    assert_eq!(result, Err(ChunkError::Sink("disk full")));
    assert_eq!(calls, 1);
}

#[test]
fn names_that_are_not_files_are_refused() {
    let bytes = testzip::valid(true);
    let package = Package::open(&bytes).unwrap();
    assert_eq!(
        pieces(&package, "bin/missing.elf"),
        Err(ChunkError::Read(ReadError::NoSuchEntry))
    );
}

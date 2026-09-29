//! `persist` — a file on the durable `/data` volume that has to survive a
//! reboot. It runs twice against the same data disk (`tools/abi/run.py` does the
//! two boots):
//!
//! * boot 1 (no file yet) creates it, `fsync`s, does positional I/O and
//!   `ftruncate`, appends, syncs again and reports `ABI:persist:WROTE`;
//! * boot 2 (file present) checks that exactly those bytes came back, removes
//!   the file, and reports `ABI:persist:PASS`.
//!
//! Which boot it is follows from the file's presence, so the program takes no
//! arguments and a manual re-run of a passed image starts over cleanly.

mod common;

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

const NAME: &str = "persist";
const FILE: &str = "/data/abi-persist.bin";
/// Several ext2 blocks, so the write crosses the direct/indirect boundary.
const LEN: usize = 14 * 1024;
/// Where `ftruncate` cuts the file.
const CUT: u64 = 9000;
const POKE_AT: u64 = 100;
const POKE: &[u8] = b"PERSIST";
const TAIL: &[u8] = b"TAIL";

fn pattern() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(31).wrapping_add(7)) as u8).collect()
}

/// The bytes the file holds when boot 1 is done.
fn expected() -> Vec<u8> {
    let mut bytes = pattern();
    bytes.truncate(CUT as usize);
    let at = POKE_AT as usize;
    bytes[at..at + POKE.len()].copy_from_slice(POKE);
    bytes.extend_from_slice(TAIL);
    bytes
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what.to_string())
    }
}

fn io<T>(result: std::io::Result<T>, what: &str) -> Result<T, String> {
    result.map_err(|error| format!("{what}: {error}"))
}

fn first_boot() -> Result<(), String> {
    let mut file = io(
        OpenOptions::new().read(true).write(true).create_new(true).open(FILE),
        "create",
    )?;
    io(file.write_all(&pattern()), "write")?;
    io(file.sync_all(), "fsync")?;

    // pwrite/pread neither depend on nor move the file position.
    io(file.write_all_at(POKE, POKE_AT), "pwrite")?;
    let mut got = [0u8; 7];
    io(file.read_exact_at(&mut got, POKE_AT), "pread")?;
    check(got == POKE, "pread returned other bytes than pwrite wrote")?;
    check(
        io(file.stream_position(), "tell")? == LEN as u64,
        "positional I/O moved the file position",
    )?;

    io(file.set_len(CUT), "ftruncate")?;
    io(file.sync_data(), "fdatasync")?;
    check(io(fs::metadata(FILE), "stat")?.len() == CUT, "size after ftruncate")?;
    io(file.seek(SeekFrom::Start(0)), "seek")?;
    let mut body = Vec::new();
    io(file.read_to_end(&mut body), "read back")?;
    check(body.len() == CUT as usize, "read back the wrong length")?;
    drop(file);

    let mut tail = io(OpenOptions::new().append(true).open(FILE), "open for append")?;
    io(tail.write_all(TAIL), "append")?;
    io(tail.sync_all(), "fsync after append")?;
    check(io(fs::read(FILE), "re-read")? == expected(), "contents before the reboot differ")?;
    println!("ABI:{NAME}:WROTE");
    Ok(())
}

fn second_boot() -> Result<(), String> {
    let bytes = io(fs::read(FILE), "read after reboot")?;
    check(bytes.len() == expected().len(), "length after reboot differs")?;
    check(bytes == expected(), "contents after reboot differ")?;
    io(fs::remove_file(FILE), "remove")?;
    check(!Path::new(FILE).exists(), "the file is still there after remove")?;
    Ok(())
}

fn main() {
    if !Path::new("/data").is_dir() {
        common::fail(NAME, "no /data volume (attach a data disk)");
    }
    let result = if Path::new(FILE).exists() {
        second_boot().map(|()| common::pass(NAME))
    } else {
        first_boot()
    };
    if let Err(reason) = result {
        common::fail(NAME, &reason);
    }
}

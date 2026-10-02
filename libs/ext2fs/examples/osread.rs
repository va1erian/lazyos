//! Read the OS volume of a built `target/lazyos.img` from the host, read-only,
//! with the library the kernel mounts it with. The harnesses use it to judge
//! what only root may read inside the guest (`/logs` is 0750 root), from
//! outside it, after QEMU has exited.
//!
//! ```text
//! cargo run -q -p ext2fs --example osread -- IMAGE cat PATH
//! cargo run -q -p ext2fs --example osread -- IMAGE stat PATH    # mode uid gid size
//! cargo run -q -p ext2fs --example osread -- IMAGE ls PATH
//! cargo run -q -p ext2fs --example osread -- IMAGE fsck /     # the library's checker
//! ```
//!
//! `fsck` runs `ext2fs::check::fsck` (the host suite's fsck-style checker,
//! behind the crate's `fuzz` feature: add `--features fuzz`) over the whole
//! volume, for hosts without `e2fsck`; it prints each problem and fails when
//! there is one.
//!
//! `IMAGE` is the whole disk; the OS volume starts at LBA 131072 (64 MiB,
//! `build_support/os_disk.rs`), or at `--lba N` for a bare volume (`--lba 0`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use ext2fs::{BlockIo, Ext2, IoError, SECTOR_SIZE};

/// Where `cargo build` puts the OS volume inside the disk image.
const OS_START_LBA: u64 = 131_072;

/// A read-only window of a host file, starting at sector `start`.
struct Window {
    file: Mutex<File>,
    start: u64,
    sectors: u64,
}

impl BlockIo for Window {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        file.seek(SeekFrom::Start((self.start + lba) * SECTOR_SIZE as u64))
            .and_then(|_| file.read_exact(buf))
            .map_err(|_| IoError::Failed)
    }

    fn write_sectors(&self, _lba: u64, _buf: &[u8]) -> Result<(), IoError> {
        Err(IoError::ReadOnly)
    }

    fn flush(&self) -> Result<(), IoError> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        false
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut args = args.to_vec();
    let mut start = OS_START_LBA;
    if let Some(at) = args.iter().position(|arg| arg == "--lba") {
        let value = args.get(at + 1).ok_or("--lba needs a number")?;
        start = value.parse().map_err(|_| format!("bad --lba {value:?}"))?;
        args.drain(at..at + 2);
    }
    let [image, command, path] = args.as_slice() else {
        return Err(String::from(
            "usage: osread IMAGE cat|stat|ls|fsck PATH [--lba N]",
        ));
    };
    let file = File::open(image).map_err(|error| format!("{image}: {error}"))?;
    let bytes = file
        .metadata()
        .map_err(|error| format!("{image}: {error}"))?
        .len();
    let sectors = (bytes / SECTOR_SIZE as u64)
        .checked_sub(start)
        .ok_or("the image is smaller than the volume offset")?;
    if command == "fsck" {
        return fsck(image, start);
    }
    let window = Window {
        file: Mutex::new(file),
        start,
        sectors,
    };
    let volume = Ext2::open(Box::new(window), || 0).map_err(|error| format!("mount: {error:?}"))?;
    let mut out = std::io::stdout().lock();
    let result = match command.as_str() {
        "cat" => volume.read_file(path).map(|data| out.write_all(&data)),
        "stat" => volume.lookup(path).map(|meta| {
            writeln!(
                out,
                "{:o} {} {} {}",
                meta.mode & 0o7777,
                meta.uid,
                meta.gid,
                meta.size
            )
        }),
        "ls" => volume.readdir(path).map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.name != "." && entry.name != "..")
                .try_for_each(|entry| writeln!(out, "{}", entry.name))
        }),
        other => return Err(format!("unknown command {other:?}")),
    };
    result
        .map_err(|error| format!("{path}: {error:?}"))?
        .map_err(|error| format!("stdout: {error}"))
}

/// Check the volume that starts at sector `start` of `image`.
#[cfg(feature = "fuzz")]
fn fsck(image: &str, start: u64) -> Result<(), String> {
    let mut file = File::open(image).map_err(|error| format!("{image}: {error}"))?;
    file.seek(SeekFrom::Start(start * SECTOR_SIZE as u64))
        .map_err(|error| format!("{image}: {error}"))?;
    let mut volume = Vec::new();
    file.read_to_end(&mut volume)
        .map_err(|error| format!("{image}: {error}"))?;
    let problems = ext2fs::check::fsck(&volume);
    let mut out = std::io::stdout().lock();
    for problem in &problems {
        writeln!(out, "{problem}").map_err(|error| format!("stdout: {error}"))?;
    }
    if problems.is_empty() {
        writeln!(out, "clean").map_err(|error| format!("stdout: {error}"))?;
        Ok(())
    } else {
        Err(format!("{} problem(s)", problems.len()))
    }
}

#[cfg(not(feature = "fuzz"))]
fn fsck(_image: &str, _start: u64) -> Result<(), String> {
    Err(String::from("fsck needs the checker: run with --features fuzz"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("osread: {error}");
        std::process::exit(1);
    }
}

//! `fsstress` — `std::fs` beyond a single read: create/append/seek/metadata/
//! read_dir/rename/remove, `BufReader`/`BufWriter` and `io::copy`.
//!
//! Every check runs so the serial log records all the gaps a boot can show;
//! the first failure becomes the `FAIL:<reason>` the bench reports.

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

/// 8.3-safe name: the kernel's FAT reader resolves short names only.
const FILE: &str = "ABIS.TXT";
const COPY: &str = "ABIC.TXT";

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn main() {
    let mut first: Option<String> = None;

    // Directories: create_dir_all, then a read_dir of the FAT root.
    if let Err(err) = fs::create_dir_all("ABIDIR/SUB") {
        note(&mut first, format!("create_dir_all ABIDIR/SUB: {err}"));
    }
    match fs::read_dir("/") {
        Ok(entries) => {
            let names: Vec<String> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            if !names.iter().any(|name| name == "HELLO.TXT") {
                note(
                    &mut first,
                    format!("read_dir / missing HELLO.TXT: {names:?}"),
                );
            }
        }
        Err(err) => note(&mut first, format!("read_dir /: {err}")),
    }

    // Create + write + append, then a content round-trip.
    match fs::write(FILE, b"hello abi") {
        Ok(()) => match OpenOptions::new().append(true).open(FILE) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(b" world") {
                    note(&mut first, format!("append {FILE}: {err}"));
                }
            }
            Err(err) => note(&mut first, format!("open append {FILE}: {err}")),
        },
        Err(err) => note(&mut first, format!("create/write {FILE}: {err}")),
    }
    match fs::read(FILE) {
        Ok(bytes) => {
            if bytes != b"hello abi world" {
                note(&mut first, format!("read back {bytes:?}"));
            }
        }
        Err(err) => note(&mut first, format!("read {FILE}: {err}")),
    }

    // Write-mode open of an existing file: reports the read-only fd gap.
    match OpenOptions::new().write(true).open("HELLO.TXT") {
        Ok(_) => {}
        Err(err) => note(&mut first, format!("open write HELLO.TXT: {err}")),
    }

    // Seek + read a known window of the fixture text.
    match File::open("HELLO.TXT") {
        Ok(mut file) => {
            let mut window = [0u8; 5];
            let read = file
                .seek(SeekFrom::Start(6))
                .and_then(|_| file.read_exact(&mut window));
            match read {
                Ok(()) => {
                    if &window != b"from " {
                        note(&mut first, format!("seek/read got {window:?}"));
                    }
                }
                Err(err) => note(&mut first, format!("seek HELLO.TXT: {err}")),
            }
        }
        Err(err) => note(&mut first, format!("open HELLO.TXT: {err}")),
    }

    // Metadata.
    match fs::metadata("HELLO.TXT") {
        Ok(meta) => {
            if !meta.is_file() {
                note(&mut first, "HELLO.TXT metadata is not a file".to_string());
            }
            if meta.len() == 0 {
                note(&mut first, "HELLO.TXT metadata length is zero".to_string());
            }
        }
        Err(err) => note(&mut first, format!("metadata HELLO.TXT: {err}")),
    }

    // BufReader/BufWriter + io::copy, verified by re-reading the copy.
    let copied = (|| -> std::io::Result<bool> {
        let mut src = BufReader::new(File::open("HELLO.TXT")?);
        let mut dst = BufWriter::new(File::create(COPY)?);
        let written = std::io::copy(&mut src, &mut dst)?;
        dst.flush()?;
        let original = fs::read("HELLO.TXT")?;
        let duplicate = fs::read(COPY)?;
        Ok(written == original.len() as u64 && duplicate == original)
    })();
    match copied {
        Ok(true) => {}
        Ok(false) => note(&mut first, "io::copy round-trip mismatch".to_string()),
        Err(err) => note(&mut first, format!("io::copy: {err}")),
    }

    // Rename, then rename back so the checks above stay valid if it worked.
    match fs::rename("HELLO.TXT", "ABIREN.TXT") {
        Ok(()) => {
            if let Err(err) = fs::rename("ABIREN.TXT", "HELLO.TXT") {
                note(&mut first, format!("rename back: {err}"));
            }
        }
        Err(err) => note(&mut first, format!("rename HELLO.TXT: {err}")),
    }

    // Remove the scratch files (and the directory if it was created).
    match fs::remove_file(FILE) {
        Ok(()) => {}
        Err(err) => note(&mut first, format!("remove_file {FILE}: {err}")),
    }
    match fs::remove_file(COPY) {
        Ok(()) => {}
        Err(err) => note(&mut first, format!("remove_file {COPY}: {err}")),
    }
    match fs::remove_dir_all("ABIDIR") {
        Ok(()) => {}
        Err(err) => note(&mut first, format!("remove_dir_all ABIDIR: {err}")),
    }

    match first {
        Some(reason) => common::fail("fsstress", &reason),
        None => common::pass("fsstress"),
    }
}

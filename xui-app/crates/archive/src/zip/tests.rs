use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::write::ZipWriter;
use super::*;
use crate::format::Level;
use crate::writer::{EntryWriter, Meta};

/// A unique scratch file, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Scratch(
            std::env::temp_dir().join(format!("lazyarc-zip-{tag}-{}-{n}.zip", std::process::id())),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn meta() -> Meta {
    Meta {
        modified: Some(1_700_000_000),
        mode: Some(0o600),
    }
}

fn sample(scratch: &Scratch, level: Level) {
    let mut writer = ZipWriter::new(BufWriter::new(File::create(&scratch.0).unwrap()), level);
    writer.dir("docs", meta()).unwrap();
    let text = "zip me ".repeat(1000);
    writer
        .file(
            "docs/a.txt",
            meta(),
            text.len() as u64,
            &mut text.as_bytes(),
        )
        .unwrap();
    writer.file("été.txt", meta(), 3, &mut &b"abc"[..]).unwrap();
    writer.symlink("docs/link", meta(), "a.txt").unwrap();
    Box::new(writer).finish().unwrap();
}

fn read(archive: &ZipArchive, scratch: &Scratch, index: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    archive
        .reader(File::open(&scratch.0).unwrap(), index)?
        .read_to_end(&mut data)?;
    Ok(data)
}

#[test]
fn a_written_zip_reads_back() {
    for level in [Level::Store, Level::Normal] {
        let scratch = Scratch::new("round");
        sample(&scratch, level);
        let archive = ZipArchive::open(&mut File::open(&scratch.0).unwrap()).unwrap();
        let paths: Vec<_> = archive.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["docs", "docs/a.txt", "été.txt", "docs/link"]);
        assert!(archive.entries[0].kind.is_dir());
        assert_eq!(archive.entries[1].size, 7000);
        assert_eq!(archive.entries[1].mode, Some(0o600));
        assert_eq!(archive.entries[1].modified, Some(1_700_000_000));
        assert_eq!(
            read(&archive, &scratch, 1).unwrap(),
            "zip me ".repeat(1000).as_bytes()
        );
        assert_eq!(read(&archive, &scratch, 2).unwrap(), b"abc");
        assert!(matches!(archive.entries[3].kind, EntryKind::Symlink { .. }));
        assert_eq!(read(&archive, &scratch, 3).unwrap(), b"a.txt");
        let method = &archive.entries[1].method;
        assert_eq!(
            method,
            if level == Level::Store {
                "Store"
            } else {
                "Deflate"
            }
        );
    }
}

#[test]
fn a_flipped_data_byte_fails_the_crc() {
    let scratch = Scratch::new("crc");
    sample(&scratch, Level::Store);
    let mut bytes = fs::read(&scratch.0).unwrap();
    let at = bytes.windows(7).position(|w| w == b"zip me ").unwrap();
    bytes[at] = b'Z';
    fs::write(&scratch.0, &bytes).unwrap();
    let archive = ZipArchive::open(&mut File::open(&scratch.0).unwrap()).unwrap();
    assert!(matches!(
        read(&archive, &scratch, 1),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn a_stub_before_the_archive_is_skipped() {
    let scratch = Scratch::new("stub");
    sample(&scratch, Level::Normal);
    let mut bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    bytes.extend(fs::read(&scratch.0).unwrap());
    fs::write(&scratch.0, &bytes).unwrap();
    let archive = ZipArchive::open(&mut File::open(&scratch.0).unwrap()).unwrap();
    assert_eq!(read(&archive, &scratch, 2).unwrap(), b"abc");
}

#[test]
fn garbage_is_not_a_zip() {
    let scratch = Scratch::new("garbage");
    fs::write(&scratch.0, b"PK\x03\x04 nothing else here").unwrap();
    assert!(ZipArchive::open(&mut File::open(&scratch.0).unwrap()).is_err());
}

#[test]
fn a_truncated_directory_is_corrupt() {
    let scratch = Scratch::new("trunc");
    sample(&scratch, Level::Normal);
    let bytes = fs::read(&scratch.0).unwrap();
    // Cut the archive inside its central directory, keeping a valid EOCD.
    let eocd = bytes.len() - 22;
    let mut cut = bytes[..eocd - 40].to_vec();
    cut.extend_from_slice(&bytes[eocd..]);
    fs::write(&scratch.0, &cut).unwrap();
    assert!(ZipArchive::open(&mut File::open(&scratch.0).unwrap()).is_err());
}

#[test]
fn raw_copies_keep_the_data_bit_for_bit() {
    let source = Scratch::new("raw-src");
    sample(&source, Level::Max);
    let archive = ZipArchive::open(&mut File::open(&source.0).unwrap()).unwrap();
    let copy = Scratch::new("raw-dst");
    let mut writer = ZipWriter::new(
        BufWriter::new(File::create(&copy.0).unwrap()),
        Level::Normal,
    );
    for (index, member) in archive.members.iter().enumerate() {
        let mut file = File::open(&source.0).unwrap();
        let start = archive.data_offset(&mut file, index).unwrap();
        file.seek(SeekFrom::Start(start)).unwrap();
        writer
            .raw_copy(member, archive.entries[index].modified, &mut file)
            .unwrap();
    }
    writer.finish_inner().unwrap().flush().unwrap();
    let copied = ZipArchive::open(&mut File::open(&copy.0).unwrap()).unwrap();
    assert_eq!(copied.entries, archive.entries);
    assert_eq!(
        read(&copied, &copy, 1).unwrap(),
        "zip me ".repeat(1000).as_bytes()
    );
}

#[test]
fn many_members_round_trip() {
    let scratch = Scratch::new("many");
    let mut writer = ZipWriter::new(
        BufWriter::new(File::create(&scratch.0).unwrap()),
        Level::Fast,
    );
    for i in 0..300 {
        let body = format!("file {i}");
        writer
            .file(
                &format!("d/{i}.txt"),
                meta(),
                body.len() as u64,
                &mut Cursor::new(body.into_bytes()),
            )
            .unwrap();
    }
    Box::new(writer).finish().unwrap();
    let archive = ZipArchive::open(&mut File::open(&scratch.0).unwrap()).unwrap();
    assert_eq!(archive.entries.len(), 300);
    assert_eq!(read(&archive, &scratch, 299).unwrap(), b"file 299");
}

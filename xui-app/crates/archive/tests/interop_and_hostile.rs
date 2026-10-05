//! Archives other tools made, and archives built to escape their folder.

mod common;

use std::fs;
use std::io::{BufWriter, Write};

use common::{files, fixture, progress, tree, Scratch};
use lazyarc::extract::{self, Options};
use lazyarc::tar::write::TarWriter;
use lazyarc::writer::{EntryWriter, FinishOut, Meta};
use lazyarc::zip::write::ZipWriter;
use lazyarc::{codec, Archive, EntryKind, Format, Level};

fn numbers() -> Vec<u8> {
    (0..4000)
        .map(|i| format!("number {i}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn reference_tool_archives_read_back() {
    for (name, format) in [
        ("7zip.zip", Format::Zip),
        ("pax.tar", Format::Tar),
        ("pax.tar.gz", Format::TarGz),
        ("pax.tar.xz", Format::TarXz),
    ] {
        let archive = Archive::open(&fixture(name), &progress()).unwrap();
        assert_eq!(archive.format, format, "{name}");
        let files = files(&archive);
        let names: Vec<_> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            ["tree/empty.txt", "tree/hello.txt", "tree/sub/numbers.txt"],
            "{name}"
        );
        assert_eq!(files[2].1, numbers(), "{name}");
        assert!(
            extract::test(&archive, &progress())
                .unwrap()
                .skipped
                .is_empty(),
            "{name}"
        );
    }
}

#[test]
fn single_compressed_files_read_back() {
    let xz = Archive::open(&fixture("hello.txt.xz"), &progress()).unwrap();
    assert_eq!(xz.format, Format::Xz);
    assert_eq!(xz.entries[0].path, "hello.txt");
    assert_eq!(files(&xz)[0].1, b"Hello from 7-Zip!\n");
    let gz = Archive::open(&fixture("numbers.txt.gz"), &progress()).unwrap();
    assert_eq!(gz.format, Format::Gz);
    // gzip stored the original name in its header.
    assert_eq!(gz.entries[0].path, "numbers.txt");
    assert_eq!(gz.entries[0].size, numbers().len() as u64);
}

#[test]
fn an_unsupported_zip_method_fails_only_its_members() {
    let archive = Archive::open(&fixture("deflate64.zip"), &progress()).unwrap();
    let scratch = Scratch::new("deflate64");
    let report = extract::extract(
        &archive,
        &|_| true,
        &scratch.0,
        &Options::default(),
        &progress(),
    )
    .unwrap();
    // The empty file and folders still come out; Deflate64 members are reported.
    assert!(scratch.join("tree/empty.txt").is_file());
    assert!(report
        .skipped
        .iter()
        .any(|(path, why)| path == "tree/sub/numbers.txt" && why.contains("Deflate64")));
}

fn meta() -> Meta {
    Meta {
        modified: Some(0),
        mode: Some(0o644),
    }
}

/// A hand-built tarball with `fill`'s members.
fn hostile_tar(scratch: &Scratch, fill: impl FnOnce(&mut dyn EntryWriter)) -> std::path::PathBuf {
    let path = scratch.join("hostile.tar");
    let file = BufWriter::new(fs::File::create(&path).unwrap());
    let out = codec::encoder(codec::Codec::None, Level::Normal, Box::new(file)).unwrap();
    let mut writer: Box<dyn EntryWriter> = Box::new(TarWriter::new(FinishOut(out)));
    fill(writer.as_mut());
    writer.finish().unwrap();
    path
}

#[test]
fn traversal_names_never_leave_the_folder() {
    let scratch = Scratch::new("slip");
    let archive_path = hostile_tar(&scratch, |w| {
        w.file("../escaped.txt", meta(), 1, &mut &b"x"[..]).unwrap();
        w.file("/abs.txt", meta(), 1, &mut &b"x"[..]).unwrap();
        w.file("ok/fine.txt", meta(), 2, &mut &b"ok"[..]).unwrap();
    });
    let archive = Archive::open(&archive_path, &progress()).unwrap();
    let out = scratch.join("out");
    let report =
        extract::extract(&archive, &|_| true, &out, &Options::default(), &progress()).unwrap();
    assert!(!scratch.join("escaped.txt").exists());
    assert_eq!(report.skipped.len(), 2);
    assert_eq!(tree(&out), vec![("ok/fine.txt".to_owned(), b"ok".to_vec())]);
}

#[test]
fn a_symlink_cannot_be_used_to_write_outside() {
    let scratch = Scratch::new("symlink-escape");
    let outside = scratch.join("outside");
    fs::create_dir_all(&outside).unwrap();
    // A link pointing out, then a file "through" it.
    let archive_path = hostile_tar(&scratch, |w| {
        w.symlink("evil", meta(), "../outside").unwrap();
        w.file("evil/planted.txt", meta(), 1, &mut &b"x"[..])
            .unwrap();
        w.symlink("inside", meta(), "ok.txt").unwrap();
        w.file("ok.txt", meta(), 1, &mut &b"y"[..]).unwrap();
    });
    let archive = Archive::open(&archive_path, &progress()).unwrap();
    let out = scratch.join("out");
    let report =
        extract::extract(&archive, &|_| true, &out, &Options::default(), &progress()).unwrap();
    assert!(!outside.join("planted.txt").exists());
    assert!(report.skipped.iter().any(|(path, _)| path == "evil"));
    // The planted file went into a real folder `evil` inside, never through a link.
    assert!(fs::symlink_metadata(out.join("evil"))
        .map(|m| !m.file_type().is_symlink())
        .unwrap_or(true));
}

#[test]
fn a_zip_bomb_header_does_not_allocate_its_claimed_size() {
    // A member claiming 4 GiB uncompressed with 5 bytes of data: the reader
    // must fail on the size mismatch, not allocate.
    let scratch = Scratch::new("bomb");
    let path = scratch.join("bomb.zip");
    let mut writer = ZipWriter::new(
        BufWriter::new(fs::File::create(&path).unwrap()),
        Level::Store,
    );
    writer.file("a.txt", meta(), 5, &mut &b"hello"[..]).unwrap();
    writer.finish_inner().unwrap().flush().unwrap();
    let mut bytes = fs::read(&path).unwrap();
    // Patch the central directory's uncompressed size to 0xfffffff0.
    let cd = bytes.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
    bytes[cd + 24..cd + 28].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    let archive = Archive::open(&path, &progress()).unwrap();
    let report = extract::test(&archive, &progress()).unwrap();
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn zip_symlinks_list_their_targets() {
    let scratch = Scratch::new("ziplink");
    let path = scratch.join("links.zip");
    let mut writer = ZipWriter::new(
        BufWriter::new(fs::File::create(&path).unwrap()),
        Level::Normal,
    );
    writer.symlink("link", meta(), "target.txt").unwrap();
    writer.finish_inner().unwrap().flush().unwrap();
    let archive = Archive::open(&path, &progress()).unwrap();
    assert_eq!(
        archive.entries[0].kind,
        EntryKind::Symlink {
            target: "target.txt".into()
        }
    );
}

#[test]
fn a_link_through_another_link_is_refused() {
    // `a/b -> .` stays inside on its own, but `c -> a/b/../..` read through
    // it resolves to the destination's parent.
    let scratch = Scratch::new("link-chain");
    let archive_path = hostile_tar(&scratch, |w| {
        w.dir("a", meta()).unwrap();
        w.symlink("a/b", meta(), ".").unwrap();
        w.symlink("c", meta(), "a/b/../..").unwrap();
    });
    let archive = Archive::open(&archive_path, &progress()).unwrap();
    let out = scratch.join("out");
    let report =
        extract::extract(&archive, &|_| true, &out, &Options::default(), &progress()).unwrap();
    assert!(report
        .skipped
        .iter()
        .any(|(path, why)| path == "c" && why.contains("through another link")));
    assert!(fs::symlink_metadata(out.join("c")).is_err());
}

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use super::write::TarWriter;
use super::*;
use crate::codec::{encoder, Codec};
use crate::format::Level;
use crate::writer::{EntryWriter, FinishOut, Meta};

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A tarball built by the writer, as bytes.
fn build(fill: impl FnOnce(&mut dyn EntryWriter)) -> Vec<u8> {
    let shared = Shared::default();
    let out = encoder(Codec::None, Level::Normal, Box::new(shared.clone())).unwrap();
    let mut writer: Box<dyn EntryWriter> = Box::new(TarWriter::new(FinishOut(out)));
    fill(writer.as_mut());
    writer.finish().unwrap();
    let bytes = shared.0.lock().unwrap().clone();
    bytes
}

/// Every member as `(entry, data)`.
fn read_all(bytes: &[u8]) -> Result<Vec<(Entry, Vec<u8>)>> {
    let mut out = Vec::new();
    walk(&mut Cursor::new(bytes), &mut |entry, data| {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        out.push((entry, buf));
        Ok(Flow::Continue)
    })?;
    Ok(out)
}

fn meta(t: i64) -> Meta {
    Meta {
        modified: Some(t),
        mode: Some(0o640),
    }
}

#[test]
fn members_round_trip() {
    let bytes = build(|w| {
        w.dir("docs", meta(10)).unwrap();
        w.file("docs/a.txt", meta(20), 5, &mut &b"hello"[..])
            .unwrap();
        w.symlink("docs/link", meta(30), "a.txt").unwrap();
    });
    let members = read_all(&bytes).unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].0.path, "docs");
    assert!(members[0].0.kind.is_dir());
    assert_eq!(members[1].0.path, "docs/a.txt");
    assert_eq!(members[1].1, b"hello");
    assert_eq!(members[1].0.modified, Some(20));
    assert_eq!(members[1].0.mode, Some(0o640));
    assert_eq!(
        members[2].0.kind,
        EntryKind::Symlink {
            target: "a.txt".into()
        }
    );
    assert_eq!(
        members.iter().map(|m| m.0.index).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn long_and_unicode_names_survive_through_pax() {
    let long = format!("{}/é-{}.txt", "deep".repeat(40), "n".repeat(150));
    let bytes = build(|w| {
        w.file(&long, meta(-5), 2, &mut &b"ok"[..]).unwrap();
    });
    let members = read_all(&bytes).unwrap();
    assert_eq!(members[0].0.path, long);
    assert_eq!(members[0].0.modified, Some(-5));
    assert_eq!(members[0].1, b"ok");
}

#[test]
fn a_file_that_lies_about_its_size_is_refused() {
    let shared = Shared::default();
    let out = encoder(Codec::None, Level::Normal, Box::new(shared)).unwrap();
    let mut writer = TarWriter::new(FinishOut(out));
    assert!(writer.file("a", meta(0), 10, &mut &b"short"[..]).is_err());
}

#[test]
fn a_callback_may_leave_data_unread() {
    let bytes = build(|w| {
        w.file("a", meta(0), 700, &mut &[7u8; 700][..]).unwrap();
        w.file("b", meta(0), 1, &mut &b"z"[..]).unwrap();
    });
    let mut names = Vec::new();
    walk(&mut Cursor::new(&bytes), &mut |entry, _data| {
        names.push(entry.path);
        Ok(Flow::Continue)
    })
    .unwrap();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn stop_ends_the_walk_early() {
    let bytes = build(|w| {
        w.file("a", meta(0), 1, &mut &b"1"[..]).unwrap();
        w.file("b", meta(0), 1, &mut &b"2"[..]).unwrap();
    });
    let mut seen = 0;
    walk(&mut Cursor::new(&bytes), &mut |_, _| {
        seen += 1;
        Ok(Flow::Stop)
    })
    .unwrap();
    assert_eq!(seen, 1);
}

#[test]
fn a_damaged_header_is_corrupt() {
    let mut bytes = build(|w| {
        w.file("a", meta(0), 1, &mut &b"1"[..]).unwrap();
    });
    bytes[10] ^= 0x55;
    assert!(matches!(read_all(&bytes), Err(Error::Corrupt(_))));
}

#[test]
fn truncated_data_is_corrupt() {
    let bytes = build(|w| {
        w.file("a", meta(0), 2000, &mut &[1u8; 2000][..]).unwrap();
    });
    assert!(read_all(&bytes[..1200]).is_err());
}

#[test]
fn a_missing_end_marker_is_tolerated() {
    let bytes = build(|w| {
        w.file("a", meta(0), 3, &mut &b"abc"[..]).unwrap();
    });
    let members = read_all(&bytes[..1024]).unwrap();
    assert_eq!(members[0].1, b"abc");
}

#[test]
fn an_oversized_pax_header_is_refused_without_allocating() {
    let block = super::write::build_header("PaxHeaders/x", "", b'x', 1 << 30, 0o644, 0, "");
    assert!(matches!(read_all(&block), Err(Error::Corrupt(_))));
}

#[test]
fn traversal_names_are_flagged() {
    let bytes = build(|w| {
        w.file("../../evil", meta(0), 1, &mut &b"x"[..]).unwrap();
    });
    let members = read_all(&bytes).unwrap();
    assert!(members[0].0.unsafe_path);
}

#[test]
fn base256_sizes_parse() {
    let mut field = [0u8; 12];
    field[0] = 0x80;
    field[11] = 0x10;
    field[10] = 0x01;
    assert_eq!(parse_number(&field), Some(0x110));
    assert_eq!(parse_octal(b"0000644 \0"), Some(0o644));
    assert_eq!(parse_octal(b"9"), None);
}

#[test]
fn a_size_that_overflows_its_padding_is_corrupt_not_a_panic() {
    // A global pax header claiming u64::MAX bytes (GNU base-256).
    let mut block = super::write::build_header("g", "", b'g', 0, 0o644, 0, "");
    block[124] = 0x80;
    block[125..128].fill(0);
    block[128..136].fill(0xff);
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert!(matches!(read_all(&block), Err(Error::Corrupt(_))));
}

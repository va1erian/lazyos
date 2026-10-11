use std::collections::BTreeMap;
use std::format;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

use super::*;

/// An in-memory firmware tree that counts what was asked of it.
#[derive(Default)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    /// A size reported in place of the real one (a file changing under us).
    lie_about_size: Option<u64>,
    sizes: usize,
    reads: usize,
}

impl Tree {
    fn with(name: &str, bytes: Vec<u8>) -> Tree {
        let mut tree = Tree::default();
        tree.files.insert(path_of(name).unwrap(), bytes);
        tree
    }
}

impl Reader for Tree {
    fn size(&mut self, path: &str) -> Result<u64, i64> {
        self.sizes += 1;
        let file = self.files.get(path).ok_or(2)?;
        Ok(self.lie_about_size.unwrap_or(file.len() as u64))
    }

    fn read(&mut self, path: &str, max: usize, out: &mut Vec<u8>) -> Result<(), i64> {
        self.reads += 1;
        let file = self.files.get(path).ok_or(2)?;
        if file.len() > max {
            return Err(27);
        }
        out.clear();
        out.extend_from_slice(file);
        Ok(())
    }
}

const FW: &str = "rtw88/rtw8822c_fw.bin";

#[test]
fn the_rtw88_firmware_and_linux_style_names_are_accepted() {
    for name in [
        FW,
        "rtw88/LICENCE.rtlwifi_firmware.txt",
        "mediatek/mt7925/WIFI_RAM_CODE_MT7925_1_1.bin",
        "ath11k/WCN6855/hw2.0/amss.bin",
        "a/b",
        "v-1/f_2.x-y",
    ] {
        assert_eq!(check_name(name), Ok(()), "{name}");
    }
    assert_eq!(
        path_of(FW).unwrap(),
        format!("{}/{FW}", fhs::FIRMWARE)
    );
}

#[test]
fn names_that_could_leave_the_tree_are_refused() {
    let cases: &[(&str, NameError)] = &[
        ("", NameError::Empty),
        ("/rtw88/x.bin", NameError::Absolute),
        ("/etc/passwd", NameError::Absolute),
        ("rtw88.bin", NameError::NoVendor),
        ("../x/y", NameError::Dot),
        ("rtw88/../../etc/shadow", NameError::Dot),
        ("rtw88/..", NameError::Dot),
        ("rtw88/./x.bin", NameError::Dot),
        ("rtw88/.hidden", NameError::Dot),
        ("rtw88//x.bin", NameError::EmptySegment),
        ("rtw88/", NameError::EmptySegment),
        ("rtw88\\..\\x", NameError::BadByte(b'\\')),
        ("rtw88/x y.bin", NameError::BadByte(b' ')),
        ("rtw88/x\0.bin", NameError::BadByte(0)),
        ("rtw88/x:y", NameError::BadByte(b':')),
        ("rtw88/caf\u{e9}.bin", NameError::BadByte(0xC3)),
        ("a/b/c/d/e/f/g", NameError::TooDeep),
    ];
    for (name, want) in cases {
        assert_eq!(check_name(name), Err(*want), "{name:?}");
        assert_eq!(path_of(name), Err(*want), "{name:?}");
    }
    let long_segment = format!("v/{}", "x".repeat(MAX_SEGMENT + 1));
    assert_eq!(check_name(&long_segment), Err(NameError::LongSegment));
    let long = format!("{}/x", "abcdefgh/".repeat(MAX_NAME / 9 + 1));
    assert_eq!(check_name(&long), Err(NameError::TooLong));
}

#[test]
fn a_file_loads_whole() {
    let blob: Vec<u8> = (0..=255u8).cycle().take(202_600).collect();
    let mut tree = Tree::with(FW, blob.clone());
    assert_eq!(load(&mut tree, FW, DEFAULT_MAX_BYTES), Ok(blob));
    assert_eq!((tree.sizes, tree.reads), (1, 1));
}

#[test]
fn the_cap_is_checked_before_anything_is_read() {
    let mut tree = Tree::with(FW, vec![7; 4097]);
    assert_eq!(
        load(&mut tree, FW, 4096),
        Err(Error::TooLarge { len: 4097, max: 4096 })
    );
    assert_eq!(tree.reads, 0, "an oversized file is never read");
    assert_eq!(load(&mut tree, FW, 4097).map(|b| b.len()), Ok(4097));
}

#[test]
fn missing_empty_and_changing_files_are_errors() {
    let mut tree = Tree::default();
    assert_eq!(load(&mut tree, FW, 64), Err(Error::Io(2)));
    let mut empty = Tree::with(FW, Vec::new());
    assert_eq!(load(&mut empty, FW, 64), Err(Error::Empty));
    assert_eq!(empty.reads, 0);
    let mut grown = Tree::with(FW, vec![1; 40]);
    grown.lie_about_size = Some(32);
    assert_eq!(
        load(&mut grown, FW, 64),
        Err(Error::Changed {
            expected: 32,
            read: 40
        })
    );
    let mut shrunk = Tree::with(FW, vec![1; 20]);
    shrunk.lie_about_size = Some(32);
    assert!(matches!(load(&mut shrunk, FW, 64), Err(Error::Changed { .. })));
}

#[test]
fn a_bad_name_never_reaches_the_reader() {
    let mut tree = Tree::default();
    assert_eq!(
        load(&mut tree, "../../etc/shadow", 64),
        Err(Error::Name(NameError::Dot))
    );
    assert_eq!((tree.sizes, tree.reads), (0, 0));
}

#[test]
fn errors_read_as_sentences() {
    let text = Error::TooLarge { len: 9, max: 8 }.to_string();
    assert_eq!(text, "9 bytes, over the 8-byte cap");
    assert!(Error::Name(NameError::Dot).to_string().contains("'.'"));
}

/// Seeded soak: any accepted name, however built, composes a path that
/// stays a plain descendant of the firmware tree.
#[test]
fn seeded_names_never_escape() {
    const ALPHABET: &[u8] = b"ab./_-\\: \0Z9.";
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut accepted = 0;
    for _ in 0..50_000 {
        let len = (next() % 24) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
            .collect();
        let Ok(name) = core::str::from_utf8(&bytes) else {
            continue;
        };
        let Ok(path) = path_of(name) else {
            continue;
        };
        accepted += 1;
        let rest = path
            .strip_prefix(fhs::FIRMWARE)
            .and_then(|rest| rest.strip_prefix('/'))
            .expect("inside the tree");
        assert!(rest.split('/').count() >= 2, "{path}");
        for segment in rest.split('/') {
            assert!(!segment.is_empty() && !segment.starts_with('.'), "{path}");
            assert!(!segment.contains(['\\', '\0', ' ', ':']), "{path}");
        }
    }
    assert!(accepted > 100, "the soak exercised the accept path: {accepted}");
}

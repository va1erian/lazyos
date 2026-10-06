//! Host tests of the in-memory tree's rules (capacity, rename, setattr)
//! and the seeded soak of the serve loop against garbage requests.

extern crate std;

use super::{clock, path_request, tree, Script};
use crate::daemon::{serve_one, Buffers, FuseFs, Target};
use crate::memfs::MemFs;
use crate::payload::{decode_dirents, set, SetAttrRecord};
use crate::wire::*;

#[test]
fn capacity_and_node_limits() {
    let mut fs = MemFs::new(10_000, 3, 0, 0, clock);
    fs.create("a", 0o644, 0, 0).unwrap();
    assert_eq!(fs.write(Target::Path("a"), 0, &[1; 10_000]), Ok(10_000));
    assert_eq!(
        fs.write(Target::Path("a"), 10_000, &[1]),
        Err(errno::ENOSPC)
    );
    assert_eq!(
        fs.write(Target::Path("a"), u64::MAX, &[1]),
        Err(errno::EINVAL)
    );
    assert_eq!(fs.truncate(Target::Path("a"), u64::MAX), Err(errno::ENOSPC));
    assert_eq!(
        fs.write(Target::Path("a"), 1 << 40, &[1]),
        Err(errno::ENOSPC)
    );
    fs.truncate(Target::Path("a"), 10).unwrap();
    assert_eq!(fs.used(), 10);
    fs.mkdir("d", 0o755, 0, 0).unwrap();
    assert_eq!(fs.create("b", 0o644, 0, 0), Err(errno::ENOSPC));
    assert_eq!(fs.create("a/x", 0o644, 0, 0), Err(errno::ENOTDIR));
    assert_eq!(fs.create("a", 0o644, 0, 0), Err(errno::EEXIST));
}

#[test]
fn rename_rules() {
    let mut fs = tree();
    fs.mkdir("d", 0o755, 0, 0).unwrap();
    fs.mkdir("d/e", 0o755, 0, 0).unwrap();
    fs.create("f", 0o644, 0, 0).unwrap();
    fs.create("g", 0o644, 0, 0).unwrap();
    fs.write(Target::Path("g"), 0, b"gg").unwrap();
    assert_eq!(fs.rename("d", "d/e/x"), Err(errno::EINVAL));
    assert_eq!(fs.rename("f", "d"), Err(errno::EISDIR));
    assert_eq!(fs.rename("d/e", "f"), Err(errno::ENOTDIR));
    assert_eq!(fs.rename("missing", "x"), Err(errno::ENOENT));
    // A file over a file replaces it and frees its bytes.
    fs.rename("f", "g").unwrap();
    assert_eq!(fs.used(), 0);
    assert_eq!(fs.lookup(Target::Path("f")), Err(errno::ENOENT));
    fs.rename("g", "g").unwrap();
    fs.rename("d", "dd").unwrap();
    assert!(fs.lookup(Target::Path("dd/e")).unwrap().is_dir());
}

#[test]
fn setattr_applies_only_selected_fields() {
    let mut fs = tree();
    fs.create("f", 0o644, 5, 6).unwrap();
    let change = SetAttrRecord {
        mask: set::MODE | set::ATIME,
        mode: 0o4600,
        uid: 99,
        atime: 42,
        ..SetAttrRecord::default()
    };
    let attr = fs.setattr("f", &change).unwrap();
    assert_eq!(attr.mode & 0o7777, 0o4600);
    assert_eq!((attr.uid, attr.atime, attr.mtime), (5, 42, clock()));
}

/// A tiny deterministic generator for the seeded soak.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn seeded_garbage_never_panics() {
    let seed = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x5eed_f05e);
    let cases: usize = std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let mut rng = Rng(seed);
    let mut fs = tree();
    let names = ["", "a", "b", "a/b", "a/c", "b/x", "..", "a//", "c"];
    for _ in 0..cases {
        let path = names[rng.next() as usize % names.len()];
        let mut request = path_request(rng.next(), Op::Lookup, path);
        request.op = (rng.next() % 15) | ((rng.next() & 1) << 16);
        request.ino = rng.next() % 8;
        request.generation = rng.next() % 8;
        request.offset = rng.next() % 200_000;
        request.len = rng.next() % 80_000;
        request.mode = rng.next();
        if rng.next().is_multiple_of(16) {
            request.path_len = rng.next() % 9000;
        }
        let mut payload = path.as_bytes().to_vec();
        payload.resize(
            payload.len() + (rng.next() % 300) as usize,
            rng.next() as u8,
        );
        let mut script = Script::default();
        script.queue.push_back((request, payload));
        serve_one(&mut fs, &mut script, &mut Buffers::new()).unwrap();
        let (reply, data) = &script.replies[0];
        assert!(data.len() <= MAX_DATA);
        if reply.status == 0 && request.operation() == Some(Op::ReadDir) {
            assert!(
                decode_dirents(data, reply.count as usize).is_some(),
                "seed {seed:#x}"
            );
        }
    }
}

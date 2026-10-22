//! Seeded fuzz: random bytes and damaged logs through the parser and the loader, and random
//! sequences of changes through the store with its invariants checked after every step.
//! `FUZZ_CASES` / `FUZZ_SEED` as in `fuzzkit`.

use std::fs;

use fuzzkit::{for_seeds, Rng};

use super::log::{self, Op, MAGIC};
use super::model::{
    clean_url, is_root, Entry, Kind, BAR, MAX_DEPTH, MAX_TITLE, MAX_URL, OTHER, UNSORTED,
};
use super::tests::Dir;
use super::Bookmarks;

fn text(rng: &mut Rng, max: u64) -> String {
    let len = rng.below(max);
    (0..len)
        .map(|_| match rng.below(12) {
            0 => '\u{202E}',
            1 => '\n',
            2 => 'é',
            3 => '日',
            _ => char::from(rng.range(0x20, 0x7e) as u8),
        })
        .collect()
}

fn url(rng: &mut Rng) -> String {
    match rng.below(8) {
        0 => String::from("javascript:alert(1)"),
        1 => format!("java\tscript:{}", text(rng, 10)),
        2 => text(rng, 3000),
        _ => format!("http://h{}.test/{}", rng.below(30), text(rng, 20)),
    }
}

fn entry(rng: &mut Rng, id: u64) -> Entry {
    Entry {
        id,
        parent: rng.below(14),
        kind: *rng.pick(&[Kind::Folder, Kind::Link, Kind::Separator]),
        title: text(rng, 700),
        url: url(rng),
        position: rng.below(10) as u32,
        created: rng.next_u64(),
        modified: rng.next_u64(),
    }
}

/// What must hold of any set, however it was made.
fn check(set: &Bookmarks) {
    for e in set.iter() {
        assert!(!is_root(e.id) && e.id != 0, "{e:?}");
        assert!(e.title.chars().count() <= MAX_TITLE);
        assert!(e.title.chars().all(|c| !crate::sanitize::is_unsafe(c)));
        assert!(e.url.len() <= MAX_URL);
        assert!(clean_url(&e.url).is_ok() || e.kind != Kind::Link, "{e:?}");
        if e.kind != Kind::Link {
            assert!(e.url.is_empty());
        }
        // The way up reaches a root through folders, within the bound.
        let mut at = e.parent;
        let mut steps = 1;
        while !is_root(at) {
            let up = set.get(at).unwrap_or_else(|| panic!("{e:?} has no parent"));
            assert_eq!(up.kind, Kind::Folder, "{e:?}");
            at = up.parent;
            steps += 1;
            assert!(steps <= MAX_DEPTH, "{e:?} is nested too deep or loops");
        }
    }
}

#[test]
fn the_parser_never_panics_on_garbage_and_never_reads_past_the_input() {
    for_seeds("places_parse_garbage", |_, rng| {
        let len = rng.below(400) as usize;
        let mut bytes = rng.bytes(len);
        if rng.one_in(2) {
            // Garbage after a real header, the interesting case.
            let mut with_header = MAGIC.to_vec();
            with_header.append(&mut bytes);
            bytes = with_header;
        }
        let parsed = log::parse(&bytes);
        assert!(parsed.valid_len <= bytes.len());
        assert!(parsed.ops.len() <= parsed.records);
    });
}

#[test]
fn a_damaged_log_gives_a_prefix_of_what_was_written() {
    for_seeds("places_parse_damaged", |_, rng| {
        let n = rng.below(20);
        let ops: Vec<Op> = (0..n)
            .map(|i| {
                if rng.one_in(4) {
                    Op::Delete(rng.below(30))
                } else {
                    Op::Put(Entry {
                        title: text(rng, 100),
                        url: text(rng, 100),
                        ..entry(rng, 4 + i)
                    })
                }
            })
            .collect();
        let mut bytes = MAGIC.to_vec();
        for op in &ops {
            bytes.extend(log::encode(op));
        }
        let whole = log::parse(&bytes);
        assert_eq!(whole.ops, ops, "an undamaged log reads back whole");
        assert_eq!(whole.valid_len, bytes.len());

        if rng.one_in(2) {
            let keep = rng.below(bytes.len() as u64 + 1) as usize;
            bytes.truncate(keep);
        } else if !bytes.is_empty() {
            let flips = 1 + rng.below(4) as usize;
            rng.flip_bits(&mut bytes, flips);
        }
        let parsed = log::parse(&bytes);
        assert!(parsed.valid_len <= bytes.len());
        // A flipped bit in a payload is caught by its checksum, in a length by the frame, so what
        // survives is a prefix; a flip that lands in the header makes it foreign instead.
        if !parsed.foreign {
            assert!(parsed.ops.len() <= ops.len());
            assert_eq!(parsed.ops[..], ops[..parsed.ops.len()]);
        }
    });
}

#[test]
fn a_store_opened_on_any_bytes_keeps_its_invariants_and_stays_writable() {
    for_seeds("places_open_garbage", |_, rng| {
        let dir = Dir::new();
        let mut bytes = MAGIC.to_vec();
        for i in 0..rng.below(40) {
            let id = 4 + rng.below(50) + i;
            bytes.extend(log::encode(&if rng.one_in(5) {
                Op::Delete(id)
            } else {
                Op::Put(entry(rng, id))
            }));
        }
        if rng.one_in(3) {
            let n = rng.below(30) as usize;
            bytes.extend(rng.bytes(n));
        }
        if rng.one_in(3) {
            let cut = rng.below(bytes.len() as u64 + 1) as usize;
            bytes.truncate(cut);
        }
        fs::write(dir.file(), &bytes).unwrap();
        let mut set = Bookmarks::open(dir.file());
        check(&set);
        // Whatever the file held, the next change is kept.
        let id = set
            .add_link(UNSORTED, "after", "http://after.test/", 1)
            .unwrap();
        check(&set);
        let back = Bookmarks::open(dir.file());
        check(&back);
        assert_eq!(
            back.get(id).map(|e| e.url.as_str()),
            Some("http://after.test/")
        );
    });
}

#[test]
fn random_changes_keep_the_set_valid_and_reopen_to_the_same_set() {
    for_seeds("places_random_changes", |_, rng| {
        let dir = Dir::new();
        let mut set = Bookmarks::open(dir.file());
        let mut now = 1;
        for _ in 0..rng.below(120) {
            now += 1;
            let ids: Vec<u64> = set.iter().map(|e| e.id).collect();
            let pick = |rng: &mut Rng| {
                if ids.is_empty() || rng.one_in(4) {
                    *rng.pick(&[BAR, OTHER, UNSORTED, 99, 0])
                } else {
                    *rng.pick(&ids)
                }
            };
            // Refusals are fine; a panic or a broken set is not.
            let _ = match rng.below(7) {
                0 | 1 => set
                    .add_link(pick(rng), &text(rng, 600), &url(rng), now)
                    .map(drop),
                2 => set.add_folder(pick(rng), &text(rng, 40), now).map(drop),
                3 => set.add_separator(pick(rng), now).map(drop),
                4 => set.edit(pick(rng), &text(rng, 40), &url(rng), now),
                5 => set.move_to(pick(rng), pick(rng), rng.below(8) as usize, now),
                _ => set.remove(pick(rng)).map(drop),
            };
            check(&set);
        }
        let back = Bookmarks::open(dir.file());
        check(&back);
        let ours: Vec<&Entry> = set.iter().collect();
        let theirs: Vec<&Entry> = back.iter().collect();
        assert_eq!(ours, theirs, "reopening gives back what was kept");
    });
}

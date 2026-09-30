//! FAT VFAT long file names (issue #414): valid runs of every shape, and every
//! way a run can be inconsistent. A bad run must never surface as a name; the
//! entry keeps its 8.3 name instead.

use super::fat_image::*;
use super::*;
use crate::fs::vfs::Filesystem;
use alloc::vec::Vec;

const NAME_TEXT: &str = "abcdefghij";

/// Names `readdir` reports for `path`, in on-disk order.
fn names(fs: &dyn Filesystem, path: &str) -> Result<Vec<String>, String> {
    let entries = fs.readdir(path).map_err(fs_error)?;
    Ok(entries.into_iter().map(|entry| entry.name).collect())
}

/// A name of exactly `len` characters.
fn name_of_len(len: usize) -> String {
    NAME_TEXT.chars().cycle().take(len).collect()
}

/// One-slot and multi-slot names round-trip, and lookups ignore ASCII case,
/// accept a leading slash, and still find an entry by its 8.3 alias.
pub fn fat_lfn_single_and_multi_entry() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let content = b"long name payload";
    let file = img.store_file(content);
    let mut root = Vec::new();
    root.extend(long_entry("Notes.md", &s83("NOTES~1.MD"), ATTR_FILE, 0, 0));
    root.extend(long_entry(
        "A rather long file name.txt",
        &s83("ARATHE~1.TXT"),
        ATTR_FILE,
        file,
        content.len() as u32,
    ));
    root.push(short_slot(&s83("PLAIN.TXT"), ATTR_FILE, 0, 0));
    root.extend(long_entry("busybox", &s83("BUSYBOX"), ATTR_FILE, 0, 0));
    img.set_root(&root);
    let fs = img.mount("test-fat-lfn-basic");

    let want = [
        "Notes.md",
        "A rather long file name.txt",
        "PLAIN.TXT",
        "busybox",
    ];
    check!(
        names(&fs, "/")? == want,
        "root listing is {:?}",
        names(&fs, "/")?
    );

    let exact = fs.lookup("Notes.md").map_err(fs_error)?;
    for spelling in [
        "notes.md",
        "NOTES.MD",
        "/Notes.md",
        "NOTES~1.MD",
        "notes~1.md",
    ] {
        let meta = fs.lookup(spelling).map_err(fs_error)?;
        check!(
            meta.ino == exact.ino,
            "{spelling} resolved to another inode"
        );
    }
    check!(
        fs.lookup("A RATHER LONG FILE NAME.TXT").is_ok(),
        "case-insensitive multi-entry lookup failed"
    );
    // The old behaviour callers rely on: a lowercase request finds an
    // uppercase 8.3 entry, and the reverse.
    check!(
        fs.lookup("plain.txt").is_ok(),
        "/plain.txt did not find PLAIN.TXT"
    );
    check!(fs.lookup("BUSYBOX").is_ok(), "BUSYBOX did not find busybox");
    check!(
        fs.lookup("Notes").err() == Some(FsError::NotFound),
        "a prefix of a long name matched"
    );

    let mut buf = [0u8; 64];
    let read = fs
        .read("a rather LONG file name.txt", 0, &mut buf)
        .map_err(fs_error)?;
    check!(
        &buf[..read] == content,
        "long-name file read {:?}",
        &buf[..read]
    );
    Ok(())
}

/// Names that exactly fill one or two slots (no terminator, no padding), the
/// 255-unit maximum, and one unit past it (refused: falls back to 8.3).
pub fn fat_lfn_boundary_lengths() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let mut root = Vec::new();
    let lengths = [(1, "ONE~1.BIN"), (13, "THIRT~1.BIN"), (26, "TWOSIX~1.BIN")];
    for (len, short) in lengths {
        root.extend(long_entry(&name_of_len(len), &s83(short), ATTR_FILE, 0, 0));
    }
    root.extend(long_entry(
        &name_of_len(255),
        &s83("MAX255~1.BIN"),
        ATTR_FILE,
        0,
        0,
    ));
    root.extend(long_entry(
        &name_of_len(256),
        &s83("OVER256.BIN"),
        ATTR_FILE,
        0,
        0,
    ));
    img.set_root(&root);
    let fs = img.mount("test-fat-lfn-lengths");

    let listed = names(&fs, "/")?;
    check!(listed.len() == 5, "listing has {} entries", listed.len());
    for (index, len) in [1usize, 13, 26, 255].into_iter().enumerate() {
        check!(
            listed[index] == name_of_len(len),
            "the {len}-character name came back as {:?}",
            listed[index]
        );
        check!(
            fs.lookup(&name_of_len(len)).is_ok(),
            "the {len}-character name does not look up"
        );
    }
    check!(
        listed[4] == "OVER256.BIN",
        "a 256-character name was accepted: {:?}",
        listed[4]
    );
    check!(
        fs.lookup(&name_of_len(256)).err() == Some(FsError::NotFound),
        "the refused 256-character name still resolves"
    );
    check!(
        fs.lookup("OVER256.BIN").is_ok(),
        "the 8.3 fallback is missing"
    );
    Ok(())
}

/// One malformed-run scenario: the slots to place in the root, the long name
/// that must NOT resolve, and the 8.3 name the entry must keep.
struct Case {
    discarded: &'static str,
    keeps: &'static str,
    slots: Vec<Slot>,
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// A malformed run of `text` for the entry `short`, mutated by `damage`.
fn damaged(text: &'static str, short: &'static str, damage: impl FnOnce(&mut Vec<Slot>)) -> Case {
    let key = s83(short);
    let mut run = lfn_run(&units(text), checksum(&key));
    damage(&mut run);
    run.push(short_slot(&key, ATTR_FILE, 0, 0));
    Case {
        discarded: text,
        keeps: short,
        slots: run,
    }
}

const THIRTY: &str = "thirty characters long name ok";

fn malformed_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    cases.push(damaged("bad checksum name here", "BADSUM~1.TXT", |run| {
        for slot in run.iter_mut() {
            slot[13] ^= 1;
        }
    }));
    cases.push(damaged(THIRTY, "MISSNG~1.TXT", |run| {
        run.remove(1); // sequence 3, then 1
    }));
    cases.push(damaged(
        "out of order names need three slots",
        "ORDER~1.TXT",
        |run| run.reverse(),
    ));
    // A 39-character name has three slots; swap the lower two.
    cases.push(damaged(
        "swapped sequence numbers in a run",
        "SWAPPD~1.TXT",
        |run| {
            run.swap(1, 2);
        },
    ));
    cases.push(damaged("no last flag on the run", "NOLAST~1.TXT", |run| {
        run[0][0] &= !0x40;
    }));
    cases.push(damaged(
        "duplicate sequence numbers ok",
        "DUPSEQ~1.TXT",
        |run| {
            run[1][0] = run[0][0] & !0x40;
        },
    ));
    cases.push(damaged("nonzero type byte here", "BADTYP~1.TXT", |run| {
        run[0][12] = 1;
    }));
    cases.push(damaged("stray cluster field set", "BADCLU~1.TXT", |run| {
        run[0][26] = 7;
    }));
    cases.push(damaged(
        "checksum changes mid-run in a long name",
        "MIDSUM~1.TXT",
        |run| {
            run[1][13] ^= 0x55;
        },
    ));
    cases.push(damaged("slash/in/name", "SLASH~1.TXT", |_| {}));
    let mut lone_high = lfn_run(&[0x61, 0xD800, 0x62], checksum(&s83("HIGH~1.TXT")));
    let mut lone_low = lfn_run(&[0xDC00, 0x61], checksum(&s83("LOW~1.TXT")));
    let mut swapped_pair = lfn_run(&[0xDC00, 0xD800], checksum(&s83("PAIR~1.TXT")));
    let mut empty = lfn_run(&[0], checksum(&s83("EMPTY~1.TXT")));
    for (run, short, text) in [
        (&mut lone_high, "HIGH~1.TXT", "a\u{fffd}b"),
        (&mut lone_low, "LOW~1.TXT", "\u{fffd}a"),
        (&mut swapped_pair, "PAIR~1.TXT", "\u{fffd}\u{fffd}"),
        (&mut empty, "EMPTY~1.TXT", ""),
    ] {
        run.push(short_slot(&s83(short), ATTR_FILE, 0, 0));
        cases.push(Case {
            discarded: text,
            keeps: short,
            slots: core::mem::take(run),
        });
    }
    cases
}

/// Every inconsistent run is discarded and the entry falls back to its 8.3
/// name; the discarded text never resolves.
pub fn fat_lfn_malformed_runs_fall_back() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let mut root = Vec::new();
    let cases = malformed_cases();
    for case in &cases {
        root.extend(case.slots.iter().copied());
    }

    // An orphaned run before a deleted entry must not attach to the live
    // entry that follows, even though its checksum matches that entry.
    let live = s83("LIVE.TXT");
    root.extend(lfn_run(&units("orphaned before deleted"), checksum(&live)));
    let mut deleted = short_slot(&s83("GONE.TXT"), ATTR_FILE, 0, 0);
    deleted[0] = 0xE5;
    root.push(deleted);
    root.push(short_slot(&live, ATTR_FILE, 0, 0));

    // A run followed by a volume label is orphaned the same way.
    let after_label = s83("LABELD.TXT");
    root.extend(lfn_run(&units("run then a label"), checksum(&after_label)));
    root.push(short_slot(&s83("MYVOLUME"), 0x08, 0, 0));
    root.push(short_slot(&after_label, ATTR_FILE, 0, 0));

    // A well-formed name with a surrogate pair still decodes.
    root.extend(long_entry(
        "smile \u{1F600}.txt",
        &s83("SMILE~1.TXT"),
        ATTR_FILE,
        0,
        0,
    ));
    img.set_root(&root);
    let fs = img.mount("test-fat-lfn-malformed");

    let listed = names(&fs, "/")?;
    let mut want: Vec<String> = cases.iter().map(|case| String::from(case.keeps)).collect();
    want.extend(["LIVE.TXT", "LABELD.TXT", "smile \u{1F600}.txt"].map(String::from));
    check!(listed == want, "listing is\n{listed:?}\nwanted\n{want:?}");

    for case in &cases {
        check!(
            fs.lookup(case.discarded).err() == Some(FsError::NotFound) || case.discarded.is_empty(),
            "the discarded name {:?} still resolves",
            case.discarded
        );
        check!(
            fs.lookup(case.keeps).is_ok(),
            "the 8.3 name {} no longer resolves",
            case.keeps
        );
    }
    check!(
        fs.lookup("orphaned before deleted").err() == Some(FsError::NotFound),
        "an orphaned run attached across a deleted entry"
    );
    check!(
        fs.lookup("run then a label").err() == Some(FsError::NotFound),
        "an orphaned run attached across a label"
    );
    check!(
        fs.lookup("SMILE \u{1F600}.TXT").is_ok(),
        "the surrogate-pair name does not look up"
    );
    Ok(())
}

/// The units after the terminator must be 0xFFFF padding: a run with anything
/// else there is discarded (the entry keeps its 8.3 name), while correctly
/// padded runs, including a 2-slot one, still decode.
pub fn fat_lfn_terminator_padding_checked() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let mut root = Vec::new();

    // "padded name" is 11 units: terminator at 11, padding at 12.
    root.extend(long_entry(
        "padded name",
        &s83("GOODPD~1.TXT"),
        ATTR_FILE,
        0,
        0,
    ));
    let bad = s83("BADPAD~1.TXT");
    let mut run = lfn_run(&units("padded name"), checksum(&bad));
    // Unit 12 lives in the last two bytes of the slot.
    run[0][30..32].copy_from_slice(&0x0041u16.to_le_bytes());
    root.extend(run);
    root.push(short_slot(&bad, ATTR_FILE, 0, 0));
    // Garbage after the terminator in the last slot of a 2-slot run.
    let bad2 = s83("BADPD2~1.TXT");
    let mut run = lfn_run(&units("a name needing two slots"), checksum(&bad2));
    run[0][30..32].copy_from_slice(&0x1234u16.to_le_bytes());
    root.extend(run);
    root.push(short_slot(&bad2, ATTR_FILE, 0, 0));
    root.extend(long_entry(
        "a name needing two slots",
        &s83("TWOSLT~1.TXT"),
        ATTR_FILE,
        0,
        0,
    ));
    img.set_root(&root);
    let fs = img.mount("test-fat-lfn-padding");

    let listed = names(&fs, "/")?;
    let want = [
        "padded name",
        "BADPAD~1.TXT",
        "BADPD2~1.TXT",
        "a name needing two slots",
    ];
    check!(listed == want, "listing is {listed:?}");
    check!(
        fs.lookup("BADPAD~1.TXT").is_ok(),
        "the 8.3 name of the bad-padding run is gone"
    );
    Ok(())
}

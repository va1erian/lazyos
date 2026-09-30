//! FAT long file names and subdirectories (issue #414).
//!
//! Every image here is synthetic and hand-built, so each case can corrupt
//! exactly one thing: a checksum, a sequence number, a chain pointer. The
//! reader must answer with the short name, an error, or a bounded walk --
//! never a panic or an unbounded loop.

use super::fat_image::*;
use super::*;
use crate::fs::fat::Fat16;
use crate::fs::overlay::Overlay;
use crate::fs::vfs::Filesystem;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

/// One-entry, 13-, 26- and 255-character names resolve by long and short
/// name, ASCII case-insensitively, and `readdir` reports them as stored.
pub fn fat_lfn_names_assemble() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    let long255: String = (0..255).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    let cases: [(&str, &[u8; 11]); 5] = [
        ("Readme.markdown", b"README~1MD "),
        ("thirteen.txt!", b"THIRTE~1TXT"),
        ("twenty-six-characters-long", b"TWENTY~1   "),
        ("Ünï-çödé ✓.txt", b"UNI~1   TXT"),
        (&long255, b"AAAAAA~1   "),
    ];
    for (i, (long, short)) in cases.iter().enumerate() {
        let start = vol.file_data(alloc::format!("file{i}").as_bytes());
        vol.add(&mut root, Some(long), short, ATTR_FILE, start, 5);
    }
    check!(
        cases[1].0.len() == 13 && cases[2].0.len() == 26,
        "case lengths"
    );
    let fs = open(&vol, "test-lfn-names");
    let mut want: Vec<String> = cases.iter().map(|(l, _)| l.to_string()).collect();
    want.sort();
    check!(
        names(&fs, "/")? == want,
        "readdir names differ: {:?}",
        names(&fs, "/")?
    );
    for (i, (long, short)) in cases.iter().enumerate() {
        let body = alloc::format!("file{i}");
        check!(
            read_all(&fs, &alloc::format!("/{long}"))? == body.as_bytes(),
            "long {i}"
        );
        let upper = alloc::format!("/{}", long.to_ascii_uppercase());
        check!(read_all(&fs, &upper)? == body.as_bytes(), "upper {i}");
        let short_name = short.iter().map(|&b| b as char).collect::<String>();
        let (base, ext) = short_name.split_at(8);
        let spelled = alloc::format!("/{}.{}", base.trim_end(), ext.trim_end());
        let spelled = spelled.trim_end_matches('.');
        check!(
            read_all(&fs, spelled)? == body.as_bytes(),
            "short {i}: {spelled}"
        );
    }
    Ok(())
}

/// A damaged run falls back to the short name and never yields the long one.
pub fn fat_lfn_inconsistent_runs_fall_back() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    let long = "a-rather-long-name-over-13.txt";
    let short_a = *b"BADSUM  TXT";
    let short_b = *b"NOSEQ   TXT";
    let short_c = *b"ORDER   TXT";
    let short_d = *b"ORPHAN  TXT";
    let short_e = *b"SURR    TXT";
    let short_f = *b"SLASH   TXT";
    let short_g = *b"NULMID  TXT";
    let short_ok = *b"GOOD    TXT";

    // Bad checksum on every fragment.
    let mut frags = lfn_slots(long, &short_a);
    frags.iter_mut().for_each(|f| f[13] ^= 0x55);
    vol.put_slots(&mut root, &frags);
    vol.put_slots(&mut root, &[short_slot(&short_a, ATTR_FILE, 0, 0)]);

    // Missing middle fragment (sequence jumps from 3 to 1).
    let long_three = "x".repeat(30);
    let mut frags = lfn_slots(&long_three, &short_b);
    frags.remove(1);
    vol.put_slots(&mut root, &frags);
    vol.put_slots(&mut root, &[short_slot(&short_b, ATTR_FILE, 0, 0)]);

    // Fragments out of order.
    let mut frags = lfn_slots(&long_three, &short_c);
    frags.swap(1, 2);
    vol.put_slots(&mut root, &frags);
    vol.put_slots(&mut root, &[short_slot(&short_c, ATTR_FILE, 0, 0)]);

    // An orphaned run before a deleted entry must not name the next entry.
    vol.put_slots(&mut root, &lfn_slots(long, &short_d));
    let mut deleted = short_slot(b"DELETED TXT", ATTR_FILE, 0, 0);
    deleted[0] = 0xE5;
    vol.put_slots(&mut root, &[deleted, short_slot(&short_d, ATTR_FILE, 0, 0)]);

    // A lone high surrogate.
    let mut units: Vec<u16> = "surrogate-".encode_utf16().collect();
    units.push(0xD800);
    let frags = lfn_slots_units(&units, checksum(&short_e));
    vol.put_slots(&mut root, &frags);
    vol.put_slots(&mut root, &[short_slot(&short_e, ATTR_FILE, 0, 0)]);

    // A `/` inside the name.
    vol.add(&mut root, Some("evil/name.txt"), &short_f, ATTR_FILE, 0, 0);

    // Data after the NUL terminator that is not padding.
    let mut units: Vec<u16> = "nul".encode_utf16().collect();
    units.extend([0, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49]);
    vol.put_slots(&mut root, &lfn_slots_units(&units, checksum(&short_g)));
    vol.put_slots(&mut root, &[short_slot(&short_g, ATTR_FILE, 0, 0)]);

    // A well-formed entry afterwards still gets its long name.
    vol.add(&mut root, Some(long), &short_ok, ATTR_FILE, 0, 0);

    let fs = open(&vol, "test-lfn-bad");
    let mut want: Vec<String> = [
        "BADSUM.TXT",
        "NOSEQ.TXT",
        "ORDER.TXT",
        "ORPHAN.TXT",
        "SURR.TXT",
        "SLASH.TXT",
        "NULMID.TXT",
        long,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    want.sort();
    let got = names(&fs, "/")?;
    check!(got == want, "names differ: {got:?}");
    check!(fs.lookup("/evil/name.txt").is_err(), "a `/` name resolved");
    check!(
        fs.lookup("/BADSUM.TXT").is_ok()
            && fs.lookup(&alloc::format!("/{}", "x".repeat(30))).is_err(),
        "fallback lookups"
    );
    Ok(())
}

/// Nested directories, a directory spread over fragmented clusters, unique
/// inodes for equal names, and the `NotDir`/`IsDir` errors.
pub fn fat_subdirectories_resolve() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    // /Docs/Deep Dir/Deeper/note.txt (long names at every level).
    let deeper_c = vol.alloc(1)[0];
    let mut deeper = vol.dir_over(&[deeper_c]);
    let note = vol.file_data(b"nested note");
    vol.add(
        &mut deeper,
        Some("note.txt"),
        b"NOTE    TXT",
        ATTR_FILE,
        note,
        11,
    );
    let deep_c = vol.alloc(1)[0];
    let mut deep = vol.dir_over(&[deep_c]);
    vol.add(
        &mut deep,
        Some("Deeper"),
        b"DEEPER     ",
        ATTR_DIR,
        deeper_c,
        0,
    );
    vol.add(&mut deep, None, b".          ", ATTR_DIR, deep_c, 0);
    vol.add(&mut deep, None, b"..         ", ATTR_DIR, 0, 0);
    let docs_c = vol.alloc(1)[0];
    let mut docs = vol.dir_over(&[docs_c]);
    vol.add(
        &mut docs,
        Some("Deep Dir"),
        b"DEEPDI~1   ",
        ATTR_DIR,
        deep_c,
        0,
    );
    vol.add(&mut root, Some("Docs"), b"DOCS       ", ATTR_DIR, docs_c, 0);

    // A directory over interleaved (fragmented) clusters holding 60 files.
    let base = vol.next;
    vol.next += 12;
    let frag: Vec<u16> = (0..12).map(|i| base + (i * 5 % 12) as u16).collect();
    let mut wide = vol.dir_over(&frag);
    for i in 0..60u32 {
        let short = alloc::format!("F{i:07}TXT");
        let long = alloc::format!("wide file number {i}.txt");
        let start = vol.file_data(alloc::format!("w{i}").as_bytes());
        let mut s = [b' '; 11];
        s.copy_from_slice(short.as_bytes());
        vol.add(
            &mut wide,
            Some(&long),
            &s,
            ATTR_FILE,
            start,
            2 + (i >= 10) as u32,
        );
    }
    vol.add(
        &mut root,
        Some("wide"),
        b"WIDE       ",
        ATTR_DIR,
        frag[0],
        0,
    );

    // Same name in two directories.
    let same_a = vol.file_data(b"A");
    let same_b = vol.file_data(b"B");
    vol.add(
        &mut docs,
        Some("same.txt"),
        b"SAME    TXT",
        ATTR_FILE,
        same_a,
        1,
    );
    vol.add(
        &mut root,
        Some("same.txt"),
        b"SAME    TXT",
        ATTR_FILE,
        same_b,
        1,
    );

    let fs = open(&vol, "test-subdirs");
    let path = "/docs/deep dir/DEEPER/Note.TXT";
    check!(read_all(&fs, path)? == b"nested note", "nested read");
    check!(
        names(&fs, "/Docs/Deep Dir")? == ["Deeper"],
        "dot entries not hidden"
    );
    check!(names(&fs, "/wide")?.len() == 60, "wide dir lost entries");
    for i in [0u32, 9, 10, 31, 59] {
        let want = alloc::format!("w{i}");
        let got = read_all(&fs, &alloc::format!("/wide/wide file number {i}.txt"))?;
        check!(got.starts_with(want.as_bytes()), "wide read {i}");
    }
    let a = fs.lookup("/Docs/same.txt").map_err(|e| format!("{e:?}"))?;
    let b = fs.lookup("/same.txt").map_err(|e| format!("{e:?}"))?;
    check!(a.ino != b.ino && a.ino > 1 && b.ino > 1, "inodes collide");
    let mut seen = alloc::collections::BTreeSet::new();
    for dir in ["/", "/Docs", "/wide"] {
        for entry in fs.readdir(dir).map_err(|e| format!("{e:?}"))? {
            check!(
                seen.insert(entry.ino),
                "duplicate inode {} in {dir}",
                entry.ino
            );
        }
    }

    check!(
        fs.lookup("/same.txt/x") == Err(FsError::NotDir),
        "file used as dir"
    );
    check!(
        fs.readdir("/same.txt") == Err(FsError::NotDir),
        "readdir on a file"
    );
    let mut buf = [0u8; 4];
    check!(
        fs.read("/Docs", 0, &mut buf) == Err(FsError::IsDir),
        "read on a dir"
    );
    check!(
        fs.lookup("/Docs/..").is_err() && fs.lookup("/Docs/.").is_err(),
        "dot names visible"
    );
    check!(
        fs.lookup("/Docs/nope") == Err(FsError::NotFound),
        "missing entry"
    );
    Ok(())
}

/// A cyclic directory chain and invalid start clusters end in an error.
pub fn fat_directory_chain_corruption() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    let cyc = vol.alloc(2);
    vol.fat(cyc[1], cyc[0]); // loop back to the start
    let mut cyc_dir = vol.dir_over(&cyc);
    vol.fat(cyc[1], cyc[0]);
    for i in 0..32 {
        let short = alloc::format!("C{i:07}TXT");
        let mut s = [b' '; 11];
        s.copy_from_slice(short.as_bytes());
        vol.add(&mut cyc_dir, None, &s, ATTR_FILE, 0, 0);
    }
    vol.add(&mut root, None, b"CYCLE      ", ATTR_DIR, cyc[0], 0);
    vol.add(&mut root, None, b"ZERO       ", ATTR_DIR, 0, 0);
    vol.add(&mut root, None, b"ONE        ", ATTR_DIR, 1, 0);
    vol.add(&mut root, None, b"HUGE       ", ATTR_DIR, 0xFFF0, 0);
    let fs = open(&vol, "test-dir-corrupt");
    // The 32 entries are all there, then the loop is caught, not followed.
    check!(
        fs.readdir("/CYCLE") == Err(FsError::Invalid),
        "cycle not detected"
    );
    check!(
        fs.lookup("/CYCLE/missing") == Err(FsError::Invalid),
        "cycle lookup"
    );
    for bad in ["ZERO", "ONE", "HUGE"] {
        check!(
            fs.readdir(&alloc::format!("/{bad}")) == Err(FsError::Invalid),
            "{bad} start cluster accepted"
        );
    }
    check!(names(&fs, "/")?.len() == 4, "root damaged by bad children");
    Ok(())
}

/// A nested lower directory copies up through the overlay and unions with
/// the lower entries.
pub fn fat_overlay_copy_up_nested() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    let sub_c = vol.alloc(1)[0];
    let mut sub = vol.dir_over(&[sub_c]);
    let lower = vol.file_data(b"lower");
    vol.add(
        &mut sub,
        Some("lower file.txt"),
        b"LOWERF~1TXT",
        ATTR_FILE,
        lower,
        5,
    );
    let top_c = vol.alloc(1)[0];
    let mut top = vol.dir_over(&[top_c]);
    vol.add(&mut top, Some("Inner"), b"INNER      ", ATTR_DIR, sub_c, 0);
    vol.add(&mut root, Some("Outer"), b"OUTER      ", ATTR_DIR, top_c, 0);
    let fs = Overlay::new(alloc::sync::Arc::new(open(&vol, "test-overlay-nested")));

    fs.write("/Outer/Inner/lower file.txt", 0, b"UPPER")
        .map_err(|e| format!("copy-up write: {e:?}"))?;
    let mut buf = [0u8; 16];
    let got = fs
        .read("/Outer/Inner/lower file.txt", 0, &mut buf)
        .map_err(|e| format!("{e:?}"))?;
    check!(&buf[..got] == b"UPPER", "copy-up lost the write");
    fs.create("/Outer/Inner/new.txt", 0o644, Id::ROOT)
        .map_err(|e| format!("{e:?}"))?;
    let listing: Vec<String> = fs
        .readdir("/Outer/Inner")
        .map_err(|e| format!("{e:?}"))?
        .into_iter()
        .map(|e| e.name)
        .collect();
    check!(
        listing.len() == 2 && listing.iter().any(|n| n == "new.txt"),
        "union listing: {listing:?}"
    );
    Ok(())
}

/// Many random lookups and reads over a wide, deep tree stay correct and do
/// not grow the heap (the path cache is bounded).
pub fn fat_tree_soak() -> Result<(), String> {
    let mut vol = Vol::new();
    let mut root = vol.root();
    let mut paths: Vec<(String, Vec<u8>)> = Vec::new();
    for d in 0..6u32 {
        let c = vol.alloc(1)[0];
        let mut dir = vol.dir_over(&[c]);
        let mut sub_dirs = Vec::new();
        for s in 0..4u32 {
            let sc = vol.alloc(1)[0];
            let mut sub = vol.dir_over(&[sc]);
            for f in 0..8u32 {
                let body = alloc::format!("d{d}s{s}f{f}").into_bytes();
                let start = vol.file_data(&body);
                let short = alloc::format!("F{f:07}TXT");
                let mut sh = [b' '; 11];
                sh.copy_from_slice(short.as_bytes());
                vol.add(
                    &mut sub,
                    Some(&alloc::format!("File Number {f}")),
                    &sh,
                    ATTR_FILE,
                    start,
                    body.len() as u32,
                );
                paths.push((alloc::format!("/Dir {d}/Sub {s}/File Number {f}"), body));
            }
            sub_dirs.push((s, sc));
        }
        for (s, sc) in sub_dirs {
            let short = alloc::format!("S{s:07}   ");
            let mut sh = [b' '; 11];
            sh.copy_from_slice(short.as_bytes());
            vol.add(
                &mut dir,
                Some(&alloc::format!("Sub {s}")),
                &sh,
                ATTR_DIR,
                sc,
                0,
            );
        }
        let short = alloc::format!("D{d:07}   ");
        let mut sh = [b' '; 11];
        sh.copy_from_slice(short.as_bytes());
        vol.add(
            &mut root,
            Some(&alloc::format!("Dir {d}")),
            &sh,
            ATTR_DIR,
            c,
            0,
        );
    }
    let fs = open(&vol, "test-tree-soak");
    // Warm every allocation the reader keeps, then measure.
    for (path, body) in &paths {
        check!(&read_all(&fs, path)? == body, "warm read {path}");
    }
    let before = crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for round in 0..4000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let (path, body) = &paths[(state as usize) % paths.len()];
        let path = if round % 3 == 0 {
            path.to_ascii_uppercase()
        } else {
            path.clone()
        };
        check!(&read_all(&fs, &path)? == body, "soak read {path}");
        check!(
            fs.lookup(&alloc::format!("{path}/nope")) == Err(FsError::NotDir),
            "soak notdir"
        );
    }
    let after = crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    check!(
        after <= before + 16 * 1024,
        "heap grew from {before} to {after}"
    );
    Ok(())
}

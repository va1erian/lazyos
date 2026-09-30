//! FAT subdirectories (issue #414): nested resolution, directories that span
//! fragmented clusters, corrupt directory chains (which must end with an
//! error, never hang), inode uniqueness, and copy-up of a nested lower
//! directory through the overlay.

use super::fat_image::*;
use super::*;
use crate::fs::overlay::Overlay;
use crate::fs::vfs::Filesystem;
use alloc::sync::Arc;
use alloc::vec::Vec;

fn names(fs: &dyn Filesystem, path: &str) -> Result<Vec<String>, String> {
    let entries = fs.readdir(path).map_err(fs_error)?;
    Ok(entries.into_iter().map(|entry| entry.name).collect())
}

fn read_all(fs: &dyn Filesystem, path: &str) -> Result<Vec<u8>, String> {
    let mut buf = [0u8; 2048];
    let read = fs.read(path, 0, &mut buf).map_err(fs_error)?;
    Ok(buf[..read].to_vec())
}

const DEEP: &str = "Alpha Dir/beta/GAMMA/delta dir/deep file.txt";

/// Five levels of directories, some with long names, resolve at every depth;
/// `.` and `..` are hidden; file-as-directory is `NotDir` and
/// reading a directory is `IsDir`.
pub fn fat_dirs_nested_resolution() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let content = b"deep content";
    let file = img.store_file(content);
    let [c1, c2, c3, c4] = [(); 4].map(|_| img.alloc());
    let mut slots = dot_slots(c4, c3).to_vec();
    slots.extend(long_entry(
        "deep file.txt",
        &s83("DEEPFI~1.TXT"),
        ATTR_FILE,
        file,
        12,
    ));
    img.store_dir_in(&[c4], &slots);
    let mut slots = dot_slots(c3, c2).to_vec();
    slots.extend(long_entry("delta dir", &s83("DELTAD~1"), ATTR_DIR, c4, 0));
    img.store_dir_in(&[c3], &slots);
    let mut slots = dot_slots(c2, c1).to_vec();
    slots.push(short_slot(&s83("GAMMA"), ATTR_DIR, c3, 0));
    img.store_dir_in(&[c2], &slots);
    let mut slots = dot_slots(c1, 0).to_vec();
    slots.extend(long_entry("beta", &s83("BETA"), ATTR_DIR, c2, 0));
    img.store_dir_in(&[c1], &slots);
    let mut root = long_entry("Alpha Dir", &s83("ALPHAD~1"), ATTR_DIR, c1, 0);
    root.push(short_slot(&s83("TOP.TXT"), ATTR_FILE, 0, 0));
    img.set_root(&root);
    let fs = img.mount("test-fat-dirs-nested");

    let mut inodes = Vec::new();
    let mut prefix = String::new();
    for part in DEEP.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        for spelling in [
            prefix.clone(),
            prefix.to_ascii_uppercase(),
            prefix.to_ascii_lowercase(),
        ] {
            let meta = fs
                .lookup(&spelling)
                .map_err(|e| format!("{spelling}: {}", fs_error(e)))?;
            check!(
                meta.ino != 0 && meta.ino != 1,
                "{spelling} has inode {}",
                meta.ino
            );
            if spelling == prefix {
                inodes.push(meta.ino);
            }
            check!(
                meta.ino == *inodes.last().unwrap_or(&0),
                "{spelling} disagrees with {prefix} about its inode"
            );
        }
    }
    let mut sorted = inodes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    check!(
        sorted.len() == inodes.len(),
        "path components share inodes: {inodes:?}"
    );
    check!(
        fs.lookup(DEEP).map_err(fs_error)?.size == 12,
        "deep file size is wrong"
    );
    // Twice: the second resolution comes from the path cache.
    check!(read_all(&fs, DEEP)? == content, "deep read differs");
    check!(
        read_all(&fs, "/alpha dir/BETA/gamma/DELTA DIR/DEEP FILE.TXT")? == content,
        "cached read differs"
    );

    check!(
        names(&fs, "/")? == ["Alpha Dir", "TOP.TXT"],
        "root lists {:?}",
        names(&fs, "/")?
    );
    check!(
        names(&fs, "Alpha Dir")? == ["beta"],
        "level 1 lists {:?}",
        names(&fs, "Alpha Dir")?
    );
    check!(
        names(&fs, "alpha dir/beta")? == ["GAMMA"],
        "level 2 lists {:?}",
        names(&fs, "alpha dir/beta")?
    );
    check!(
        names(&fs, "Alpha Dir/beta/GAMMA/delta dir")? == ["deep file.txt"],
        "deepest lists wrong"
    );

    let expect = |path: &str, want: FsError| -> Result<(), String> {
        let got = fs.lookup(path).err();
        check!(
            got == Some(want),
            "lookup({path:?}) gave {got:?}, wanted {want:?}"
        );
        Ok(())
    };
    expect("Alpha Dir/nope", FsError::NotFound)?;
    expect("Alpha Dir/nope/deeper", FsError::NotFound)?;
    expect("Alpha Dir/./beta", FsError::NotFound)?;
    expect("Alpha Dir/../Alpha Dir", FsError::NotFound)?;
    expect("Alpha Dir/beta/..", FsError::NotFound)?;
    expect("TOP.TXT/child", FsError::NotDir)?;
    expect(
        "Alpha Dir/beta/GAMMA/delta dir/deep file.txt/x",
        FsError::NotDir,
    )?;
    check!(
        fs.read("Alpha Dir", 0, &mut [0u8; 4]).err() == Some(FsError::IsDir),
        "reading a directory"
    );
    check!(
        fs.read("Alpha Dir/beta", 0, &mut [0u8; 4]).err() == Some(FsError::IsDir),
        "reading a subdirectory"
    );
    check!(
        fs.read("", 0, &mut [0u8; 4]).err() == Some(FsError::IsDir),
        "reading the root"
    );
    check!(
        fs.readdir("TOP.TXT").err() == Some(FsError::NotDir),
        "readdir of a file"
    );
    check!(
        fs.readdir(DEEP).err() == Some(FsError::NotDir),
        "readdir of a deep file"
    );
    check!(
        fs.readdir("Alpha Dir/none").err() == Some(FsError::NotFound),
        "readdir of a missing dir"
    );
    check!(
        fs.lookup("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z").is_err(),
        "an absurdly deep path resolved"
    );
    Ok(())
}

/// A directory whose clusters are scattered and out of order, with long-name
/// runs that straddle cluster boundaries, lists and resolves completely.
pub fn fat_dirs_fragmented_multi_cluster() -> Result<(), String> {
    let mut img = FatImage::new(200, false);
    let scattered: Vec<u16> = (0..6).map(|_| img.alloc()).collect();
    let chain: Vec<u16> = [3, 0, 5, 1, 4, 2].iter().map(|&i| scattered[i]).collect();

    let mut slots = dot_slots(chain[0], 0).to_vec();
    let mut want = Vec::new();
    for i in 0..30 {
        let name = format!("file number {i:02}.dat");
        let payload = format!("content-{i}");
        let cluster = img.store_file(payload.as_bytes());
        let short = s83(&format!("FILE{i:02}~1.DAT"));
        slots.extend(long_entry(
            &name,
            &short,
            ATTR_FILE,
            cluster,
            payload.len() as u32,
        ));
        want.push((name, payload));
    }
    check!(
        slots.len() > 5 * SLOTS_PER_CLUSTER,
        "the directory should span 6 clusters"
    );
    img.store_dir_in(&chain, &slots);
    img.set_root(&long_entry("wide", &s83("WIDE"), ATTR_DIR, chain[0], 0));
    let fs = img.mount("test-fat-dirs-fragmented");

    let listed = names(&fs, "wide")?;
    let expected: Vec<&str> = want.iter().map(|(name, _)| name.as_str()).collect();
    check!(listed == expected, "fragmented directory lists {listed:?}");
    for (name, payload) in &want {
        let path = format!("WIDE/{}", name.to_ascii_uppercase());
        check!(
            read_all(&fs, &path)? == payload.as_bytes(),
            "{name} reads back wrong"
        );
    }
    Ok(())
}

/// Entries are packed to the last slot of a cluster with no end marker; the
/// chain's clean end finishes the listing without an error.
pub fn fat_dirs_full_cluster_ends_cleanly() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let mut slots = Vec::new();
    for i in 0..SLOTS_PER_CLUSTER {
        slots.push(short_slot(&s83(&format!("F{i:07}.TXT")), ATTR_FILE, 0, 0));
    }
    let dir = img.store_dir(&slots);
    img.set_root(&[short_slot(&s83("FULL"), ATTR_DIR, dir, 0)]);
    let fs = img.mount("test-fat-dirs-full");
    let listed = names(&fs, "FULL")?;
    check!(
        listed.len() == SLOTS_PER_CLUSTER,
        "{} entries listed",
        listed.len()
    );
    check!(
        fs.lookup("FULL/F0000015.TXT").is_ok(),
        "the last slot is unreachable"
    );
    Ok(())
}

/// Corrupt directory chains end with `Invalid`: a self-loop, a longer cycle,
/// reserved/bad/out-of-range/free pointers, and start clusters that are 0, 1
/// or past the volume. A lookup that would have to walk past the corruption
/// fails instead of answering "not found".
pub fn fat_dirs_corrupt_chains_terminate() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let full: Vec<Slot> = (0..SLOTS_PER_CLUSTER)
        .map(|i| short_slot(&s83(&format!("F{i:07}.TXT")), ATTR_FILE, 0, 0))
        .collect();
    let mut root = Vec::new();

    // Cycles of length 1, 2 and 3.
    for (label, length) in [("SELF", 1usize), ("PAIR", 2), ("TRIPLE", 3)] {
        let clusters: Vec<u16> = (0..length).map(|_| img.alloc()).collect();
        for cluster in &clusters {
            img.write_cluster(*cluster, &full.concat());
        }
        for (index, cluster) in clusters.iter().enumerate() {
            img.set_fat(*cluster, clusters[(index + 1) % length]);
        }
        root.push(short_slot(&s83(label), ATTR_DIR, clusters[0], 0));
    }
    // A bad pointer after a full first cluster.
    for (label, pointer) in [
        ("RESERVED", 1u16),
        ("BADMARK", 0xFF7),
        ("FAR", 0xF00),
        ("FREE", 0),
    ] {
        let cluster = img.alloc();
        img.write_cluster(cluster, &full.concat());
        img.set_fat(cluster, pointer);
        root.push(short_slot(&s83(label), ATTR_DIR, cluster, 0));
    }
    // Start clusters that cannot hold a directory.
    for (label, start) in [("START0", 0u16), ("START1", 1), ("STARTFAR", 0xF00)] {
        root.push(short_slot(&s83(label), ATTR_DIR, start, 0));
    }
    img.set_root(&root);
    let fs = img.mount("test-fat-dirs-corrupt");

    for label in [
        "SELF", "PAIR", "TRIPLE", "RESERVED", "BADMARK", "FAR", "FREE", "START0", "START1",
        "STARTFAR",
    ] {
        check!(fs.lookup(label).is_ok(), "{label} itself should stat");
        let listing = fs.readdir(label).err();
        check!(
            listing == Some(FsError::Invalid),
            "readdir({label}) gave {listing:?}"
        );
        let missing = fs.lookup(&format!("{label}/NOSUCH.TXT")).err();
        check!(
            missing == Some(FsError::Invalid),
            "lookup past the end of {label} gave {missing:?}"
        );
        let mut buf = [0u8; 8];
        check!(
            fs.read(&format!("{label}/NOSUCH.TXT"), 0, &mut buf)
                .is_err(),
            "read through {label} succeeded"
        );
    }
    // An entry ahead of the corruption is still reachable.
    check!(
        fs.lookup("SELF/F0000003.TXT").is_ok(),
        "an entry before the loop is lost"
    );
    Ok(())
}

/// Equal names in different directories get different inodes, and `readdir`
/// agrees with `lookup`; no entry is inode 0 and only the root is inode 1.
pub fn fat_dirs_unique_inodes() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let [x, y, xy] = [(); 3].map(|_| img.alloc());
    let same = || -> Vec<Slot> {
        let mut slots = long_entry("same.txt", &s83("SAME~1.TXT"), ATTR_FILE, 0, 0);
        slots.push(short_slot(&s83("DUP.TXT"), ATTR_FILE, 0, 0));
        slots
    };
    let mut slots = same();
    slots.push(short_slot(&s83("Y"), ATTR_DIR, xy, 0));
    img.store_dir_in(&[x], &slots);
    img.store_dir_in(&[y], &same());
    img.store_dir_in(&[xy], &same());
    let mut root = same();
    root.push(short_slot(&s83("X"), ATTR_DIR, x, 0));
    root.push(short_slot(&s83("Y"), ATTR_DIR, y, 0));
    img.set_root(&root);
    let fs = img.mount("test-fat-dirs-inodes");

    check!(
        fs.lookup("").map_err(fs_error)?.ino == 1,
        "the root is not inode 1"
    );
    let mut seen = Vec::new();
    for dir in ["", "X", "Y", "X/Y"] {
        for entry in fs.readdir(dir).map_err(fs_error)? {
            let path = if dir.is_empty() {
                entry.name.clone()
            } else {
                format!("{dir}/{}", entry.name)
            };
            let meta = fs.lookup(&path).map_err(fs_error)?;
            check!(
                meta.ino == entry.ino,
                "{path}: readdir says {} but lookup says {}",
                entry.ino,
                meta.ino
            );
            check!(entry.ino > 1, "{path} has the reserved inode {}", entry.ino);
            check!(
                !seen.contains(&entry.ino),
                "{path} reuses inode {}",
                entry.ino
            );
            seen.push(entry.ino);
        }
    }
    check!(
        seen.len() == 4 * 2 + 3,
        "expected 11 entries, saw {}",
        seen.len()
    );
    Ok(())
}

/// Copy-up of a file two directories deep lifts the whole nested lower
/// directory into the upper layer; the FAT volume itself is unchanged.
pub fn fat_dirs_overlay_copy_up_nested() -> Result<(), String> {
    let mut img = FatImage::new(64, false);
    let deep = img.store_file(b"deep lower");
    let top = img.store_file(b"top lower");
    let [docs, sub] = [(); 2].map(|_| img.alloc());
    let mut slots = dot_slots(sub, docs).to_vec();
    slots.extend(long_entry(
        "deep.txt",
        &s83("DEEP.TXT"),
        ATTR_FILE,
        deep,
        10,
    ));
    img.store_dir_in(&[sub], &slots);
    let mut slots = dot_slots(docs, 0).to_vec();
    slots.extend(long_entry("sub folder", &s83("SUBFOL~1"), ATTR_DIR, sub, 0));
    slots.extend(long_entry("top.txt", &s83("TOP.TXT"), ATTR_FILE, top, 9));
    img.store_dir_in(&[docs], &slots);
    img.set_root(&long_entry("Docs", &s83("DOCS"), ATTR_DIR, docs, 0));
    let lower = Arc::new(img.mount("test-fat-dirs-overlay"));
    let overlay = Overlay::new(lower.clone());

    check!(
        read_all(&overlay, "Docs/sub folder/deep.txt")? == b"deep lower",
        "lower read through the overlay"
    );
    check!(
        overlay
            .write("Docs/sub folder/deep.txt", 0, b"DEEP")
            .map_err(fs_error)?
            == 4,
        "the copy-up write was short"
    );
    check!(
        read_all(&overlay, "Docs/sub folder/deep.txt")? == b"DEEP lower",
        "read-your-write"
    );
    check!(
        read_all(&*lower, "Docs/sub folder/deep.txt")? == b"deep lower",
        "the FAT volume changed"
    );
    check!(
        read_all(&overlay, "docs/TOP.TXT")? == b"top lower",
        "a sibling was lost in the copy-up"
    );

    overlay
        .create("Docs/sub folder/new.txt", 0o644, Id::ROOT)
        .map_err(fs_error)?;
    let mut merged = names(&overlay, "Docs/sub folder")?;
    merged.sort();
    check!(
        merged == ["deep.txt", "new.txt"],
        "merged listing is {merged:?}"
    );
    check!(
        names(&*lower, "Docs/sub folder")? == ["deep.txt"],
        "the lower directory gained an entry"
    );
    Ok(())
}

/// A FAT16 volume resolves nested long names too (the FAT entry width is the
/// only difference), including a directory that spans several clusters.
pub fn fat_dirs_on_fat16() -> Result<(), String> {
    let mut img = FatImage::new(4300, true);
    let payload = b"sixteen bit fat payload";
    let file = img.store_file(payload);
    let mut slots = dot_slots(0, 0).to_vec();
    for i in 0..20 {
        slots.extend(long_entry(
            &format!("filler entry {i}"),
            &s83(&format!("FILL{i:02}~1")),
            ATTR_FILE,
            0,
            0,
        ));
    }
    slots.extend(long_entry(
        "payload file.bin",
        &s83("PAYLOA~1.BIN"),
        ATTR_FILE,
        file,
        payload.len() as u32,
    ));
    let dir = img.store_dir(&slots);
    img.set_root(&long_entry("Sixteen", &s83("SIXTEEN"), ATTR_DIR, dir, 0));
    let fs = img.mount("test-fat-dirs-fat16");
    check!(
        read_all(&fs, "sixteen/PAYLOAD FILE.BIN")? == payload,
        "FAT16 nested read differs"
    );
    check!(
        names(&fs, "Sixteen")?.len() == 21,
        "FAT16 directory lists {}",
        names(&fs, "Sixteen")?.len()
    );
    Ok(())
}

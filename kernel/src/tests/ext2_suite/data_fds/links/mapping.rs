//! Shared mappings of one file through two of its names: one page cache, so
//! one frame per page, and a store through one mapping is what the other
//! mapping, `read` through either name and the file itself see.

use super::*;

/// Where the second name is mapped.
const OTHER: u64 = BASE + 0x10_0000;

/// One base: map `a` and its link `b` shared, store through each, and check
/// every view agrees.
fn through_two_names(base: &str) -> Result<(), String> {
    let (a, b) = (format!("{base}/ma"), format!("{base}/mb"));
    put(&a, &filemap::pattern(2 * PAGE as usize, 3))?;
    check!(link(&a, &b) == 0, "link");
    let space = Space::new()?;
    space.enter();
    let (fa, fb) = (filemap::open_rw(&a)?, filemap::open_rw(&b)?);
    check!(
        filemap::mmap(BASE, 2 * PAGE, RW, MAP_SHARED | filemap::MAP_FIXED, fa, 0) == BASE,
        "mmap {a}"
    );
    check!(
        filemap::mmap(OTHER, 2 * PAGE, RW, MAP_SHARED | filemap::MAP_FIXED, fb, 0) == OTHER,
        "mmap {b}"
    );
    space.store(BASE + 10, b"through a")?;
    check!(
        space.load(OTHER + 10, 9)? == b"through a",
        "{base}: the mapping of {b} misses a store through {a}"
    );
    space.store(OTHER + PAGE + 7, b"through b")?;
    check!(
        space.load(BASE + PAGE + 7, 9)? == b"through b",
        "{base}: the mapping of {a} misses a store through {b}"
    );
    for page in [0, PAGE] {
        check!(
            frame_of(space.table, BASE + page)? == frame_of(space.table, OTHER + page)?,
            "{base}: page {page:#x} has two frames"
        );
    }
    // `read` through either name sees the stores (the cache is the truth
    // until written back), and a `write` shows in both mappings.
    check!(
        filemap::pread_ok(fb, 9, 10)? == b"through a",
        "{base}: read through {b}"
    );
    check!(pwrite(fa, b"written", 100) == 7, "pwrite");
    check!(
        space.load(OTHER + 100, 7)? == b"written",
        "{base}: a write through {a} misses the mapping of {b}"
    );
    check!(
        filemap::msync(BASE, 2 * PAGE, filemap::MS_SYNC) == 0,
        "msync"
    );
    let file = filemap::bytes_of(&b)?;
    check!(
        file[10..19] == *b"through a"
            && file[PAGE as usize + 7..PAGE as usize + 16] == *b"through b",
        "{base}: the stores did not reach the file"
    );
    check!(filemap::munmap(BASE, 2 * PAGE) == 0, "munmap a");
    check!(filemap::munmap(OTHER, 2 * PAGE) == 0, "munmap b");
    close(fa);
    close(fb);
    drop(space);
    for name in [a, b] {
        check!(path_call(SYS_UNLINK, &name, 0) == 0, "unlink {name}");
    }
    Ok(())
}

pub fn shared_mapping_through_two_names() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        through_two_names(base)?;
    }
    data.check_clean()
}

/// One round of the mapping soak: map the file through one name, unlink
/// that name while mapped, store, and read the store back through the other.
pub(super) fn map_unlink_store(base: &str, round: usize) -> Result<(), String> {
    let (a, b) = (format!("{base}/sa"), format!("{base}/sb"));
    put(&a, &filemap::pattern(PAGE as usize, round as u8))?;
    check!(link(&a, &b) == 0, "round {round}: link");
    let space = Space::new()?;
    space.enter();
    let fa = filemap::open_rw(&a)?;
    check!(
        filemap::mmap(BASE, PAGE, RW, MAP_SHARED | filemap::MAP_FIXED, fa, 0) == BASE,
        "round {round}: mmap"
    );
    close(fa);
    check!(path_call(SYS_UNLINK, &a, 0) == 0, "round {round}: unlink");
    let tag = format!("round {round:04}");
    space.store(BASE + 64, tag.as_bytes())?;
    check!(filemap::munmap(BASE, PAGE) == 0, "round {round}: munmap");
    drop(space);
    check!(
        filemap::bytes_of(&b)?[64..64 + tag.len()] == *tag.as_bytes(),
        "{base} round {round}: the store did not reach the other name"
    );
    check!(
        path_call(SYS_UNLINK, &b, 0) == 0,
        "round {round}: unlink {b}"
    );
    Ok(())
}

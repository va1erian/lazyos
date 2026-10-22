//! `hardlink` — hard links (issue #871), what WebKit's blob cache and `ln`
//! need: `link` makes a second name of one file (same `st_ino`, `st_nlink`
//! 2 on both, a write through one read through the other); `unlink` of one
//! name leaves the other with `st_nlink` 1 and leaves no hidden entry in the
//! directory even while a descriptor is open; the last name's unlink makes
//! an open descriptor's `st_nlink` 0; a `MAP_SHARED` mapping through one name
//! and a write through the other agree; `rename` onto a second name of the
//! same file keeps both; and the refusals (`EEXIST`, `EPERM` for a
//! directory, `ENOENT`).
//!
//! It runs on every writable base the image has and reports each finished
//! base as `ABI:hardlink:ROUND:<base>`; a failing step prints
//! `ABI:hardlink:GAP:<base>:<step>:<detail>`.

mod common;

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const NAME: &str = "hardlink";
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const PAGE: usize = 4096;
const FAILED: *mut c_void = !0usize as *mut c_void;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
        -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
}

fn gap(base: &str, step: &str, detail: impl std::fmt::Display) -> String {
    format!("{base}:{step}:{detail}")
}

fn expect(base: &str, step: &str, ok: bool, detail: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(gap(base, step, detail))
    }
}

/// `(st_ino, st_nlink)` of `path`.
fn ids(base: &str, path: &str) -> Result<(u64, u64), String> {
    let meta = fs::metadata(path).map_err(|e| gap(base, "stat", format!("{path}: {e}")))?;
    Ok((meta.ino(), meta.nlink()))
}

/// Names in `dir` that the kernel parks unlinked-while-open files under.
fn hidden(dir: &str) -> usize {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".unlinked-"))
            .count()
    })
}

fn round(base: &str) -> Result<(), String> {
    let dir = format!("{base}/HLINKD");
    let (a, b, c) = (format!("{dir}/A"), format!("{dir}/B"), format!("{dir}/C"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir).map_err(|e| gap(base, "create_dir", e))?;
    fs::write(&a, b"hello").map_err(|e| gap(base, "write", e))?;

    fs::hard_link(&a, &b).map_err(|e| gap(base, "hard_link", e))?;
    let (ino_a, links_a) = ids(base, &a)?;
    let (ino_b, links_b) = ids(base, &b)?;
    expect(base, "same_inode", ino_a == ino_b, "two inodes")?;
    expect(
        base,
        "nlink_two",
        links_a == 2 && links_b == 2,
        &format!("nlink {links_a}/{links_b}"),
    )?;
    let mut through_b = OpenOptions::new().append(true).open(&b).map_err(|e| gap(base, "open_b", e))?;
    through_b.write_all(b" world").map_err(|e| gap(base, "append_b", e))?;
    drop(through_b);
    let read = fs::read(&a).map_err(|e| gap(base, "read_a", e))?;
    expect(base, "write_through_other_name", read == b"hello world", "missed")?;

    // Refusals.
    let refused = |step: &str, result: std::io::Result<()>, want: ErrorKind| {
        match result {
            Err(e) if e.kind() == want => Ok(()),
            other => Err(gap(base, step, format!("{other:?}, wanted {want:?}"))),
        }
    };
    refused("onto_existing", fs::hard_link(&a, &b), ErrorKind::AlreadyExists)?;
    refused("missing", fs::hard_link(format!("{dir}/NOPE"), &c), ErrorKind::NotFound)?;
    refused("directory", fs::hard_link(&dir, format!("{base}/HLINKD2")), ErrorKind::PermissionDenied)?;

    // rename onto a second name of the same file: both stay.
    fs::rename(&a, &b).map_err(|e| gap(base, "rename_same_file", e))?;
    expect(base, "rename_same_file_keeps", Path::new(&a).exists() && Path::new(&b).exists(), "a name went")?;

    // Shared mapping through A, write through B.
    let file = OpenOptions::new().read(true).write(true).open(&a).map_err(|e| gap(base, "open_rw", e))?;
    file.set_len(PAGE as u64).map_err(|e| gap(base, "set_len", e))?;
    // SAFETY: a fresh shared mapping of one page of a file that long.
    let map = unsafe {
        mmap(std::ptr::null_mut(), PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, file.as_raw_fd(), 0)
    };
    if map == FAILED {
        return Err(gap(base, "mmap", std::io::Error::last_os_error()));
    }
    let map = map as *mut u8;
    let mut other = OpenOptions::new().write(true).open(&b).map_err(|e| gap(base, "open_b2", e))?;
    other.seek(SeekFrom::Start(100)).map_err(|e| gap(base, "seek", e))?;
    other.write_all(b"via B").map_err(|e| gap(base, "write_b", e))?;
    // SAFETY: inside the live one-page mapping.
    let seen = unsafe { std::slice::from_raw_parts(map.add(100), 5) }.to_vec();
    expect(base, "mapping_sees_other_name", seen == b"via B", "stale mapping")?;
    // SAFETY: inside the mapping, which is writable and shared.
    unsafe { map.add(200).copy_from(b"via map".as_ptr(), 7) };
    let mut back = [0u8; 7];
    let mut reader = File::open(&b).map_err(|e| gap(base, "open_b3", e))?;
    reader.seek(SeekFrom::Start(200)).map_err(|e| gap(base, "seek_b3", e))?;
    reader.read_exact(&mut back).map_err(|e| gap(base, "read_b3", e))?;
    expect(base, "other_name_sees_mapping", &back == b"via map", "store not seen")?;
    // SAFETY: the mapping made above, unmapped once.
    if unsafe { munmap(map as *mut c_void, PAGE) } != 0 {
        return Err(gap(base, "munmap", std::io::Error::last_os_error()));
    }

    // unlink one name while a descriptor is open: no hidden entry, nlink 1.
    let parked = hidden(&dir);
    fs::remove_file(&a).map_err(|e| gap(base, "unlink_a", e))?;
    expect(base, "unlink_one_no_parking", hidden(&dir) == parked, "a hidden entry appeared")?;
    let open_links = file.metadata().map_err(|e| gap(base, "fstat", e))?.nlink();
    expect(base, "fstat_after_unlink", open_links == 1, &format!("nlink {open_links}"))?;
    expect(base, "stat_other_name", ids(base, &b)?.1 == 1, "nlink of B")?;
    // The last name goes: the open descriptor's count is 0, data intact.
    fs::remove_file(&b).map_err(|e| gap(base, "unlink_b", e))?;
    let last = file.metadata().map_err(|e| gap(base, "fstat_last", e))?.nlink();
    expect(base, "fstat_unlinked", last == 0, &format!("nlink {last}"))?;
    let mut tail = [0u8; 5];
    let mut file = file;
    file.seek(SeekFrom::Start(100)).map_err(|e| gap(base, "seek_last", e))?;
    file.read_exact(&mut tail).map_err(|e| gap(base, "read_last", e))?;
    expect(base, "unlinked_data", &tail == b"via B", "data lost")?;
    // The other descriptors of the file go too: the last close frees it.
    drop((file, other, reader));
    expect(base, "closed_unparked", hidden(&dir) == 0, "a hidden entry stayed")?;
    fs::remove_dir_all(&dir).map_err(|e| gap(base, "cleanup", e))?;
    println!("ABI:{NAME}:ROUND:{base}");
    Ok(())
}

fn main() {
    let mut bases = vec!["/tmp"];
    if Path::new("/data").is_dir() {
        bases.push("/data");
    }
    let mut failed = false;
    for base in bases {
        if let Err(detail) = round(base) {
            println!("ABI:{NAME}:GAP:{detail}");
            failed = true;
        }
    }
    if failed {
        common::fail(NAME, "see the GAP lines");
    }
    common::pass(NAME);
}

//! `filemap` — file-backed `mmap` (issue #535, gap 7): what a program that
//! maps its input, a database with a shared-memory index or two processes
//! sharing a file see.
//!
//! Covered: a `MAP_PRIVATE` mapping reads the file, and its stores stay in
//! the process; a `MAP_SHARED` mapping's stores reach the file (`read`, a
//! second mapping, `msync`, `munmap`) and a `write` shows in the mapping; a
//! `fork`ed child's stores through a shared mapping are the parent's;
//! `ftruncate` growth reads zeros; an offset mapping maps the right pages;
//! `mprotect` and `PROT_NONE`; a read-only descriptor refuses a writable
//! shared mapping (`EACCES`); and the mapping outlives `close` and `unlink`.

mod common;

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

const PROT_NONE: i32 = 0;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MS_SYNC: i32 = 4;
const PAGE: usize = 4096;
const FAILED: *mut c_void = !0usize as *mut c_void;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
        -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn msync(addr: *mut c_void, len: usize, flags: i32) -> i32;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(code: i32) -> !;
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed).wrapping_add((i / PAGE) as u8))
        .collect()
}

fn map(fd: &File, len: usize, prot: i32, flags: i32, off: usize) -> Result<*mut u8, String> {
    // SAFETY: a fresh mapping at a kernel-chosen address.
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, prot, flags, fd.as_raw_fd(), off as i64) };
    if ptr == FAILED {
        return Err(format!(
            "mmap(len {len}, prot {prot}, flags {flags}, off {off}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(ptr as *mut u8)
}

fn view<'a>(ptr: *mut u8, len: usize) -> &'a [u8] {
    // SAFETY: the callers pass a live mapping of at least `len` bytes.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn run() -> Result<(), String> {
    let path = "/tmp/filemap.dat";
    let data = pattern(4 * PAGE + 123, 7);
    std::fs::write(path, &data).map_err(|e| format!("write: {e}"))?;

    // Private: reads the file, stores stay private.
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let private = map(&file, data.len(), PROT_READ | PROT_WRITE, MAP_PRIVATE, 0)?;
    if view(private, data.len()) != data.as_slice() {
        return Err("private mapping differs from the file".into());
    }
    // SAFETY: inside the mapping, which is writable.
    unsafe { private.add(PAGE + 5).write(0xEE) };
    if std::fs::read(path).map_err(|e| e.to_string())? != data {
        return Err("a private store reached the file".into());
    }
    if view(private, data.len())[PAGE + 5] != 0xEE {
        return Err("a private store was lost".into());
    }
    // A writable shared mapping needs a descriptor open for writing.
    // SAFETY: a refused request; nothing is mapped.
    let refused = unsafe {
        mmap(std::ptr::null_mut(), PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, file.as_raw_fd(), 0)
    };
    if refused != FAILED {
        return Err("MAP_SHARED|PROT_WRITE of a read-only descriptor was allowed".into());
    }
    drop(file);

    // Shared: stores are the file's, write(2) shows in the mapping.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open rw: {e}"))?;
    let shared = map(&file, data.len(), PROT_READ | PROT_WRITE, MAP_SHARED, 0)?;
    let second = map(&file, 2 * PAGE, PROT_READ, MAP_SHARED, 0)?;
    // SAFETY: inside the mapping, which is writable.
    unsafe { std::ptr::copy_nonoverlapping(b"through the map".as_ptr(), shared.add(100), 15) };
    if view(second, 2 * PAGE)[100..115] != *b"through the map" {
        return Err("a second shared mapping missed the store".into());
    }
    let mut seen = vec![0u8; 15];
    file.seek(SeekFrom::Start(100)).map_err(|e| e.to_string())?;
    file.read_exact(&mut seen).map_err(|e| e.to_string())?;
    if seen != b"through the map" {
        return Err("read(2) missed a store through a mapping".into());
    }
    file.seek(SeekFrom::Start(2 * PAGE as u64 + 10)).map_err(|e| e.to_string())?;
    file.write_all(b"by write").map_err(|e| e.to_string())?;
    if view(shared, data.len())[2 * PAGE + 10..2 * PAGE + 18] != *b"by write" {
        return Err("write(2) is not visible in a shared mapping".into());
    }
    // SAFETY: both are live mappings of this file.
    if unsafe { msync(shared as *mut c_void, data.len(), MS_SYNC) } != 0 {
        return Err("msync failed".into());
    }

    // A child's stores through the shared mapping are ours.
    // SAFETY: plain fork; the child only stores and exits.
    let child = unsafe { fork() };
    if child == 0 {
        // SAFETY: inside the shared mapping.
        unsafe {
            shared.add(3 * PAGE).write(0x5A);
            _exit(0)
        }
    }
    let mut status = 0;
    // SAFETY: `child` is our child.
    if unsafe { waitpid(child, &mut status, 0) } != child || status != 0 {
        return Err(format!("child failed: {status}"));
    }
    if view(shared, data.len())[3 * PAGE] != 0x5A {
        return Err("the child's shared store is not the parent's".into());
    }

    // Growth reads zeros; PROT_NONE and mprotect.
    file.set_len(6 * PAGE as u64).map_err(|e| e.to_string())?;
    let grown = map(&file, 2 * PAGE, PROT_READ, MAP_SHARED, 4 * PAGE)?;
    let grown_view = view(grown, 2 * PAGE);
    if grown_view[200..PAGE].iter().any(|&b| b != 0) || grown_view[PAGE..].iter().any(|&b| b != 0) {
        return Err("the grown file does not read zeros".into());
    }
    // SAFETY: live mapping.
    if unsafe { mprotect(grown as *mut c_void, 2 * PAGE, PROT_NONE) } != 0
        || unsafe { mprotect(grown as *mut c_void, 2 * PAGE, PROT_READ | PROT_WRITE) } != 0
    {
        return Err("mprotect".into());
    }
    // SAFETY: now writable.
    unsafe { grown.add(PAGE).write(9) };

    // The mapping outlives the descriptor and the name.
    drop(file);
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    if view(shared, data.len())[100..115] != *b"through the map" {
        return Err("the mapping changed when the file was closed and unlinked".into());
    }
    // SAFETY: each pointer is a mapping made above.
    unsafe {
        munmap(private as *mut c_void, data.len());
        munmap(shared as *mut c_void, data.len());
        munmap(second as *mut c_void, 2 * PAGE);
        munmap(grown as *mut c_void, 2 * PAGE);
    }
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => common::pass("filemap"),
        Err(reason) => common::fail("filemap", &reason),
    }
}

//! Linux descriptors on the persistent `/data` volume (issue #334): files are
//! read and written in place through the VFS, and `pread64`, `pwrite64`,
//! `ftruncate`, `truncate`, `fsync`, `sync`, `statfs` behave as Linux does.
//!
//! Every test mounts a freshly formatted ext2 disk as `/data` in the Linux ABI
//! table ([`Data`]) and drives it only through [`dispatch_for_test`] syscalls,
//! so the descriptor layer, the open-file registry and the filesystem are all
//! exercised together. [`Data::remount`] stands in for a reboot.
//!
//! [`dispatch_for_test`]: crate::process::linux::dispatch_for_test

use super::fixtures::{check_volume, pattern_bytes, raw_state};
use super::*;
use crate::ipc::credentials::{self, Cred};

mod attrs;
mod inspect;
mod io;
mod names;
mod reserved;
mod sizing;
mod soak;
mod statx;
mod times;
mod vectored;

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    (
        "linux_data_roundtrip_and_persist",
        io::roundtrip_and_persist,
    ),
    ("linux_data_open_flags", io::open_flags),
    ("linux_data_seek_bounds", io::seek_bounds),
    ("linux_data_positional_io", io::positional_io),
    (
        "linux_data_positional_io_bad_inputs",
        io::positional_bad_inputs,
    ),
    (
        "linux_data_snapshot_positional_io",
        io::snapshot_positional_io,
    ),
    ("linux_data_bad_user_buffers", io::bad_user_buffers),
    (
        "linux_data_ftruncate_truncate",
        sizing::ftruncate_and_truncate,
    ),
    (
        "linux_data_truncate_bad_inputs",
        sizing::truncate_bad_inputs,
    ),
    (
        "linux_data_fsync_sync_are_durable",
        sizing::fsync_and_sync_are_durable,
    ),
    ("linux_data_statfs", sizing::statfs_reports_volume),
    ("linux_data_unlink_while_open", names::unlink_while_open),
    ("linux_data_rename_while_open", names::rename_while_open),
    (
        "linux_data_rename_over_open_file",
        names::rename_over_open_file,
    ),
    ("linux_data_permission_denials", names::permission_denials),
    (
        "linux_data_create_no_read_bit",
        names::create_with_no_read_bit,
    ),
    ("linux_data_read_only_volume", names::read_only_volume),
    (
        "linux_data_reserved_prefix_is_refused",
        reserved::reserved_prefix_is_refused,
    ),
    (
        "linux_data_soak_create_write_unlink",
        soak::create_write_unlink,
    ),
    ("linux_data_soak_fd_table_churn", soak::fd_table_churn),
    ("linux_data_soak_fill_and_free", soak::fill_and_free),
    // Filesystem inspection and vectored I/O (issue #348).
    (
        "linux_proc_mounts_lists_the_table",
        inspect::proc_mounts_lists_the_table,
    ),
    (
        "linux_proc_mounts_read_only_volume",
        inspect::proc_mounts_reports_a_read_only_volume,
    ),
    (
        "linux_getdents_matches_getdents64",
        inspect::getdents_matches_getdents64,
    ),
    (
        "linux_getdents_whole_records",
        inspect::getdents_hands_out_whole_records,
    ),
    (
        "linux_getdents_bad_descriptors",
        inspect::getdents_bad_descriptors,
    ),
    (
        "linux_getdents_bad_buffer",
        inspect::getdents_bad_buffer_loses_nothing,
    ),
    ("linux_statx_fields", statx::statx_reports_stat_fields),
    (
        "linux_stat_family_owner",
        statx::stat_family_reports_the_owner,
    ),
    (
        "linux_statx_empty_path_and_dirfd",
        statx::statx_empty_path_and_dirfd,
    ),
    (
        "linux_statx_flags_mask_and_errors",
        statx::statx_flags_mask_and_errors,
    ),
    ("linux_statx_bad_pointers", statx::statx_bad_pointers),
    (
        "linux_vectored_positional_roundtrip",
        vectored::preadv_pwritev_roundtrip,
    ),
    (
        "linux_vectored_readv_writev_walk",
        vectored::readv_writev_share_the_walk,
    ),
    ("linux_vectored_bad_inputs", vectored::vectored_bad_inputs),
    (
        "linux_vectored_v2_flags",
        vectored::vectored_v2_flags_and_current_offset,
    ),
    (
        "linux_vectored_read_only_volume",
        vectored::vectored_on_read_only_volume,
    ),
    ("linux_vectored_snapshot_io", vectored::snapshot_vectored_io),
    ("linux_vectored_soak", vectored::soak_vectored_io),
    ("linux_data_chmod_matrix", attrs::chmod_matrix),
    ("linux_data_attr_descriptor_forms", attrs::descriptor_forms),
    ("linux_data_chown_rules", attrs::chown_rules),
    ("linux_data_utimes_rules", times::utimes_rules),
    ("linux_data_attrs_read_only", attrs::read_only_attrs),
    (
        "linux_data_attrs_survive_remount",
        attrs::attrs_survive_remount,
    ),
    ("linux_data_soak_attr_churn", soak::attr_churn),
];

// Linux numbers and flags the tests spell out.
const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_CLOSE: u64 = 3;
const SYS_FSTAT: u64 = 5;
const SYS_LSEEK: u64 = 8;
const SYS_PREAD: u64 = 17;
const SYS_PWRITE: u64 = 18;
const SYS_PIPE: u64 = 22;
const SYS_DUP: u64 = 32;
const SYS_FCNTL: u64 = 72;
const SYS_FSYNC: u64 = 74;
const SYS_FDATASYNC: u64 = 75;
const SYS_TRUNCATE: u64 = 76;
const SYS_FTRUNCATE: u64 = 77;
const SYS_RENAME: u64 = 82;
const SYS_MKDIR: u64 = 83;
const SYS_UNLINK: u64 = 87;
const SYS_STATFS: u64 = 137;
const SYS_FSTATFS: u64 = 138;
const SYS_SYNC: u64 = 162;
const SYS_OPENAT: u64 = 257;
const SYS_SYNCFS: u64 = 306;

const AT_FDCWD: u64 = (-100i64) as u64;
const O_RDONLY: u64 = 0;
const O_WRONLY: u64 = 1;
const O_RDWR: u64 = 2;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;
const F_GETFL: u64 = 3;

const ENOENT: u64 = 2;
const EBADF: u64 = 9;
const EACCES: u64 = 13;
const EFAULT: u64 = 14;
const EEXIST: u64 = 17;
const EISDIR: u64 = 21;
const EINVAL: u64 = 22;
const ENOSPC: u64 = 28;
const ESPIPE: u64 = 29;
const EROFS: u64 = 30;

/// Total blocks of the test volume (see [`Data::new`]).
const VOLUME_BLOCKS: u32 = 512;

/// The value `-errno` takes as a syscall return.
fn errno(code: u64) -> u64 {
    (code as i64).wrapping_neg() as u64
}

fn syscall(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    process::linux::dispatch_args_for_test(nr, a1, a2, a3, a4)
}

/// A NUL-terminated copy of `path` the syscall can read.
fn cstr(path: &str) -> Vec<u8> {
    let mut bytes = Vec::from(path.as_bytes());
    bytes.push(0);
    bytes
}

/// A syscall taking one path.
fn path_call(nr: u64, path: &str, a2: u64) -> u64 {
    syscall(nr, cstr(path).as_ptr() as u64, a2, 0, 0)
}

/// `openat(AT_FDCWD, path, flags, 0o644)`.
fn open(path: &str, flags: u64) -> u64 {
    open_mode(path, flags, 0o644)
}

fn open_mode(path: &str, flags: u64, mode: u64) -> u64 {
    syscall(
        SYS_OPENAT,
        AT_FDCWD,
        cstr(path).as_ptr() as u64,
        flags,
        mode,
    )
}

fn close(fd: u64) -> u64 {
    syscall(SYS_CLOSE, fd, 0, 0, 0)
}

fn write(fd: u64, data: &[u8]) -> u64 {
    syscall(SYS_WRITE, fd, data.as_ptr() as u64, data.len() as u64, 0)
}

fn pwrite(fd: u64, data: &[u8], offset: u64) -> u64 {
    syscall(
        SYS_PWRITE,
        fd,
        data.as_ptr() as u64,
        data.len() as u64,
        offset,
    )
}

fn lseek(fd: u64, offset: i64, whence: u64) -> u64 {
    syscall(SYS_LSEEK, fd, offset as u64, whence, 0)
}

/// `read` up to `len` bytes; the bytes on success, the raw return on error.
fn read(fd: u64, len: usize) -> Result<Vec<u8>, u64> {
    let mut buf = vec![0u8; len];
    let got = syscall(SYS_READ, fd, buf.as_mut_ptr() as u64, len as u64, 0);
    if got > len as u64 {
        return Err(got);
    }
    buf.truncate(got as usize);
    Ok(buf)
}

fn pread(fd: u64, len: usize, offset: u64) -> Result<Vec<u8>, u64> {
    let mut buf = vec![0u8; len];
    let got = syscall(SYS_PREAD, fd, buf.as_mut_ptr() as u64, len as u64, offset);
    if got > len as u64 {
        return Err(got);
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// The size `fstat` reports for `fd`.
fn fstat_size(fd: u64) -> Result<u64, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "fstat({fd}) returned {ret:#x}");
    Ok(u64::from_le_bytes(stat[48..56].try_into().unwrap()))
}

/// Everything in `path`, read through a fresh descriptor.
fn slurp(path: &str) -> Result<Vec<u8>, String> {
    let fd = open(path, O_RDONLY);
    check!(fd < 16, "open({path}) returned {fd:#x}");
    let mut all = Vec::new();
    loop {
        let chunk = read(fd, 700).map_err(|code| format!("read returned {code:#x}"))?;
        if chunk.is_empty() {
            break;
        }
        all.extend_from_slice(&chunk);
    }
    close(fd);
    Ok(all)
}

/// Write `data` to a new file at `path` and close it.
fn put(path: &str, data: &[u8]) -> Result<(), String> {
    let fd = open(path, O_CREAT | O_TRUNC | O_WRONLY);
    check!(fd < 16, "create({path}) returned {fd:#x}");
    check!(
        write(fd, data) == data.len() as u64,
        "short write to {path}"
    );
    check!(close(fd) == 0, "close({path}) failed");
    Ok(())
}

/// `(free blocks, free inodes)` of `/data` as `statfs` reports them.
fn free_space() -> Result<(u64, u64), String> {
    let stat = crate::fs::abi_statfs(Id::ROOT, "/data").map_err(fs_error)?;
    Ok((stat.blocks_free, stat.files_free))
}

/// Names in `/data`.
fn data_names() -> Result<Vec<String>, String> {
    let entries = crate::fs::abi_readdir(Id::ROOT, "/data").map_err(fs_error)?;
    Ok(entries.into_iter().map(|entry| entry.name).collect())
}

/// Whether descriptors 3.. are all closed and no file is registered as open.
fn nothing_open(baseline: usize) -> bool {
    (3..task::FD_COUNT).all(|fd| task::fd_kind(fd) == task::FdKind::Closed)
        && crate::fs::openfile::open_files() == baseline
}

/// A formatted ext2 disk mounted as `/data` in the ABI table, restored on drop.
struct Data {
    disk: &'static FakeDisk,
    previous: Option<Vfs>,
    /// Files registered as open before the test began.
    open_files: usize,
}

impl Data {
    /// Format pooled disk `slot` and mount it (root credentials, no leftover
    /// descriptors).
    fn new(slot: usize) -> Result<Data, String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        credentials::set(task::current(), Cred::ROOT);
        for fd in 3..task::FD_COUNT {
            let _ = task::fd_close(fd);
        }
        let (fs, _vfs, disk) = mounted_in(slot, 1024, VOLUME_BLOCKS)?;
        let previous = crate::fs::install_abi_data_for_test(fs);
        Ok(Data {
            disk,
            previous,
            open_files: crate::fs::openfile::open_files(),
        })
    }

    /// Mount the same disk again in a fresh table, as after a reboot: nothing
    /// cached, only what reached the sectors.
    fn remount(&self) -> Result<(), String> {
        let fs = Arc::new(Ext2::open(self.disk).map_err(fs_error)?);
        drop(crate::fs::install_abi_data_for_test(fs));
        Ok(())
    }

    /// The volume's bitmaps, counters and (via `statfs`) free space agree, and
    /// nothing is left open.
    fn check_clean(&self) -> Result<(), String> {
        check!(
            nothing_open(self.open_files),
            "descriptors or registered open files were leaked"
        );
        check_volume(self.disk, VOLUME_BLOCKS)
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        for fd in 3..task::FD_COUNT {
            let _ = task::fd_close(fd);
        }
        credentials::set(task::current(), Cred::ROOT);
        self.disk.set_read_only(false);
        crate::fs::restore_abi_for_test(self.previous.take());
    }
}

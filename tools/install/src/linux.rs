//! What the running Linux knows that the installer must ask it (step 1):
//! device numbers, mounts, sysfs partition facts, ESP free space. Parsing is
//! split from reading so it is tested on any host.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

pub use crate::probe::{key, parse_mountinfo, split_dev, DevNum, Mount};

/// The device number of the block device `path` (a symlink is followed).
pub fn block_dev(path: &Path) -> Result<DevNum, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_block_device() {
        return Err(format!("{} is not a block device", path.display()));
    }
    Ok(split_dev(meta.rdev()))
}

/// The device a file or directory lives on (`st_dev`).
pub fn dev_of(path: &Path) -> Result<DevNum, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(split_dev(meta.dev()))
}

pub fn mounts() -> Result<Vec<Mount>, String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
    Ok(parse_mountinfo(&text))
}

/// The devices in `/proc/swaps`.
pub fn swaps() -> Vec<DevNum> {
    let text = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
    text.lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|path| block_dev(Path::new(path)).ok())
        .collect()
}

/// A partition's facts from `/sys/dev/block/<major>:<minor>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysPart {
    /// The whole disk's kernel name (`sda`, `nvme0n1`).
    pub disk: String,
    pub index: u32,
    pub start: u64,
    pub sectors: u64,
    /// Held by device-mapper, MD or LVM.
    pub held: bool,
}

/// Read [`SysPart`] for `dev`; an error when it is not a partition.
pub fn sys_part(dev: DevNum) -> Result<SysPart, String> {
    let link = PathBuf::from(format!("/sys/dev/block/{}:{}", dev.0, dev.1));
    let dir = std::fs::canonicalize(&link).map_err(|e| format!("{}: {e}", link.display()))?;
    let read = |name: &str| -> Result<u64, String> {
        let path = dir.join(name);
        let text = std::fs::read_to_string(&path)
            .map_err(|_| format!("{} is not a partition", dir.display()))?;
        text.trim()
            .parse()
            .map_err(|_| format!("{}: not a number", path.display()))
    };
    let index = read("partition")? as u32;
    let disk = dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or("no parent disk in sysfs")?
        .to_string();
    let held = std::fs::read_dir(dir.join("holders"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    // sysfs counts 512-byte sectors whatever the logical block size.
    Ok(SysPart {
        disk,
        index,
        start: read("start")?,
        sectors: read("size")?,
        held,
    })
}

/// Whether the block device `path` is mounted or held: opening it with
/// `O_EXCL` fails with `EBUSY` then (Linux's exclusive open of a block
/// device). Any other failure is left to the checks that follow.
pub fn in_use(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EXCL)
        .open(path)
        .is_err_and(|e| e.raw_os_error() == Some(libc::EBUSY))
}

/// Free bytes of the filesystem holding `path`.
pub fn free_bytes(path: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: `statvfs` is plain old data; all zeroes is a valid value.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid nul-terminated path and `stats` a properly
    // sized, writable `statvfs` the call fills in.
    let result = unsafe { libc::statvfs(c.as_ptr(), &mut stats) };
    if result != 0 {
        return Err(format!(
            "statvfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(stats.f_bavail as u64 * stats.f_frsize as u64)
}

/// Whether the machine booted through UEFI.
pub fn booted_uefi() -> bool {
    Path::new("/sys/firmware/efi").is_dir()
}

/// `PRETTY_NAME` from `/etc/os-release`.
pub fn distro() -> String {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Whether `program` is on `PATH`.
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(program);
            std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
        })
    })
}

/// The first line a command prints, or `None` when it does not run.
pub fn first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    text.lines().next().map(str::to_string)
}

/// Whether this process runs as root.
pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

//! The disk side of the OS image: fixed geometry, the MBR entry for the OS
//! volume, and a file-backed [`BlockIo`] the build formats and updates it with.
//!
//! Geometry is fixed so an update never moves data. The bootloader writes MBR
//! entry 1 (stage 2) and entry 2 (the FAT `/boot` volume) from LBA 1; the OS
//! volume is entry 3 at [`OS_START_LBA`] (64 MiB), and the build fails if the
//! FAT volume would reach it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use ext2fs::{BlockIo, IoError};

/// Bytes per sector.
pub const SECTOR: u64 = 512;
/// Where the OS volume starts: 64 MiB, whatever the size of `/boot`.
pub const OS_START_LBA: u64 = 131_072;
/// MBR partition type of a Linux filesystem.
pub const OS_PART_TYPE: u8 = 0x83;
/// Default OS volume size (`LAZYOS_OS_SIZE`).
pub const DEFAULT_OS_SIZE: u64 = 512 << 20;
/// What `LAZYOS_SRB2KART=1` adds to [`DEFAULT_OS_SIZE`] (its game data is about
/// 460 MiB): too much for the default alone.
pub const SRB2KART_EXTRA: u64 = 512 << 20;
/// What webd (any desktop that ships it, `LAZYOS_LAZYWEB=1` included) or `LAZYOS_WEBKIT_PORT=1` add
/// to [`DEFAULT_OS_SIZE`]: the three static WebKit programs (webd, `webprocess`,
/// `networkprocess`) are about 107 MiB each and webd's package, which
/// `pkgd` unpacks into `/apps`, holds them again; 768 MiB left too little room
/// to write, with the rest of the image.
pub const WEBKIT_EXTRA: u64 = 512 << 20;
/// Smallest OS volume the build accepts.
pub const MIN_OS_SIZE: u64 = 128 << 20;

const TABLE: usize = 0x1BE;
const ENTRY: usize = 16;
const SIGNATURE: usize = 510;

/// The OS volume size when `LAZYOS_OS_SIZE` is unset: the default plus what each
/// selected feature adds. The features add up (SRB2Kart's data beside the
/// browser's programs filled a 1 GiB volume, and the update then died halfway:
/// `tools/lazygui/osvolume.py` mirrors this).
pub fn default_size(srb2kart: bool, webkit: bool) -> u64 {
    DEFAULT_OS_SIZE
        + if srb2kart { SRB2KART_EXTRA } else { 0 }
        + if webkit { WEBKIT_EXTRA } else { 0 }
}

/// [`default_size`] for the environment: `LAZYOS_SRB2KART=1`,
/// `LAZYOS_WEBKIT_PORT=1` (the console proof, with the same three programs) asks for a larger one
/// (`tools/lazygui/osvolume.py` mirrors this), and so does `webd`: a desktop that ships it, which
/// `LAZYOS_LAZYWEB=1` makes (`webd_embed::shipped_env`, which the caller asks: the installer includes this
/// file without it).
pub fn default_size_env(webd: bool) -> u64 {
    println!("cargo:rerun-if-env-changed=LAZYOS_SRB2KART");
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBKIT_PORT");
    let on = |name: &str| std::env::var_os(name).is_some_and(|value| value == "1");
    default_size(on("LAZYOS_SRB2KART"), on("LAZYOS_WEBKIT_PORT") || webd)
}

/// `LAZYOS_OS_SIZE`: bytes with an optional `K`, `M` or `G` suffix (binary),
/// at least [`MIN_OS_SIZE`], rounded down to a whole 4 KiB block.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (digits, shift) = match text.chars().last() {
        Some('K' | 'k') => (&text[..text.len() - 1], 10),
        Some('M' | 'm') => (&text[..text.len() - 1], 20),
        Some('G' | 'g') => (&text[..text.len() - 1], 30),
        _ => (text, 0),
    };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("LAZYOS_OS_SIZE={text:?} is not a size like 512M or 2G"))?;
    let bytes = count
        .checked_mul(1 << shift)
        .filter(|bytes| *bytes <= (u64::from(u32::MAX) - OS_START_LBA) * SECTOR)
        .ok_or_else(|| format!("LAZYOS_OS_SIZE={text:?} is too large"))?
        & !4095;
    if bytes < MIN_OS_SIZE {
        return Err(format!(
            "LAZYOS_OS_SIZE={text:?} is below the {}M minimum",
            MIN_OS_SIZE >> 20
        ));
    }
    Ok(bytes)
}

/// Make `file` sparse before it is extended with `set_len`.
///
/// NTFS allocates and zero-fills a dense file as far as the first write past
/// its old end, synchronously: formatting a 1 GiB volume (which writes the
/// last group's bitmaps) spent 15 s on a spinning drive writing zeros nobody
/// asked for, and the stick's 1.6 GiB image more. A sparse file allocates what
/// is written. Other hosts' file systems are sparse already, and one that
/// cannot be (FAT, exFAT) refuses the request, which is ignored.
pub fn mark_sparse(file: &File) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;

        const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
        #[link(name = "kernel32")]
        extern "system" {
            fn DeviceIoControl(
                device: *mut c_void,
                code: u32,
                input: *const c_void,
                input_len: u32,
                output: *mut c_void,
                output_len: u32,
                returned: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }
        let mut returned = 0u32;
        // SAFETY: `file` owns a valid handle for the whole call; no input or
        // output buffer and no overlapped structure are passed, and
        // `returned` is a live local the call writes its byte count to.
        let _ = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
    }
    #[cfg(not(windows))]
    let _ = file;
}

/// One MBR partition entry: (type, first LBA, sector count). `n` is 1 to 4.
pub fn mbr_entry(mbr: &[u8], n: usize) -> Option<(u8, u64, u64)> {
    let at = TABLE + (n - 1) * ENTRY;
    let raw = mbr.get(at..at + ENTRY)?;
    let word = |from: usize| u64::from(u32::from_le_bytes(raw[from..from + 4].try_into().unwrap()));
    Some((raw[4], word(8), word(12)))
}

/// Whether `mbr` carries the 0x55AA boot signature.
pub fn has_signature(mbr: &[u8]) -> bool {
    mbr.get(SIGNATURE..SIGNATURE + 2) == Some(&[0x55, 0xAA])
}

/// Fail when the FAT `/boot` volume (entry 2) ends past the OS volume's start,
/// or entry 3 is already taken.
pub fn check_boot_fits(bios: &[u8]) -> Result<(), String> {
    let (kind, start, sectors) = mbr_entry(bios, 2).ok_or("the bootloader image has no MBR")?;
    if kind == 0 || !has_signature(bios) {
        return Err("the bootloader image has no FAT partition in MBR entry 2".into());
    }
    if start + sectors > OS_START_LBA {
        return Err(format!(
            "the FAT /boot partition ends at LBA {} (past the OS volume at LBA {OS_START_LBA}, \
             64 MiB); trim what goes on /boot (LAZYOS_RAMDISK?) so the OS volume keeps its place",
            start + sectors
        ));
    }
    match mbr_entry(bios, 3) {
        Some((0, _, _)) => Ok(()),
        _ => Err("MBR entry 3 is already in use".into()),
    }
}

/// Write the OS volume's entry (3) into `mbr` for a volume of `os_bytes`.
pub fn add_os_entry(mbr: &mut [u8], os_bytes: u64) {
    let at = TABLE + 2 * ENTRY;
    let entry = &mut mbr[at..at + ENTRY];
    entry.fill(0);
    entry[4] = OS_PART_TYPE;
    entry[8..12].copy_from_slice(&(OS_START_LBA as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&((os_bytes / SECTOR) as u32).to_le_bytes());
}

/// The sector range of `file` starting at `start_lba`, `sectors` long, as a
/// [`BlockIo`]. Writes are refused when `writable` is false.
pub struct FileIo {
    file: Mutex<File>,
    start_lba: u64,
    sectors: u64,
    writable: bool,
    /// Whether [`BlockIo::flush`] waits for the host to put the data on the
    /// medium (`fsync`). Kept for `lazyos-install`, which writes a real disk.
    durable: bool,
}

impl FileIo {
    pub fn new(file: File, start_lba: u64, sectors: u64, writable: bool) -> FileIo {
        FileIo {
            file: Mutex::new(file),
            start_lba,
            sectors,
            writable,
            durable: true,
        }
    }

    /// A [`FileIo`] for a file the build makes new (a fresh image renamed into
    /// place, the stick, the RAM root): `flush` hands the data to the host's
    /// file cache and returns. The file is read back through that cache (by
    /// QEMU, by the next build), so waiting for the disk only costs time: about
    /// 17 s per 500 MiB on a spinning drive. A host crash can lose the file,
    /// which the next build makes again. Not for a volume that is updated in
    /// place: its dirty and clean markers need `flush` to be a barrier.
    pub fn volatile(file: File, start_lba: u64, sectors: u64, writable: bool) -> FileIo {
        FileIo {
            durable: false,
            ..FileIo::new(file, start_lba, sectors, writable)
        }
    }

    fn seek_to(&self, file: &mut File, lba: u64, len: usize) -> Result<(), IoError> {
        let sectors = (len as u64) / SECTOR;
        if !(len as u64).is_multiple_of(SECTOR)
            || lba.checked_add(sectors).is_none_or(|e| e > self.sectors)
        {
            return Err(IoError::Failed);
        }
        file.seek(SeekFrom::Start((self.start_lba + lba) * SECTOR))
            .map(|_| ())
            .map_err(|_| IoError::Failed)
    }
}

impl BlockIo for FileIo {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, buf.len())?;
        file.read_exact(buf).map_err(|_| IoError::Failed)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if !self.writable {
            return Err(IoError::ReadOnly);
        }
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, buf.len())?;
        file.write_all(buf).map_err(|_| IoError::Failed)
    }

    /// One seek and one read for the whole run the block cache hands over (a
    /// system call per 4 KiB block costs more than the copy).
    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), IoError> {
        let total: usize = bufs.iter().map(|buf| buf.len()).sum();
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, total)?;
        let mut run = vec![0u8; total];
        file.read_exact(&mut run).map_err(|_| IoError::Failed)?;
        let mut at = 0;
        for buf in bufs.iter_mut() {
            buf.copy_from_slice(&run[at..at + buf.len()]);
            at += buf.len();
        }
        Ok(())
    }

    /// One seek and one write for the whole run the block cache hands over.
    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), IoError> {
        if !self.writable {
            return Err(IoError::ReadOnly);
        }
        let run: Vec<u8> = bufs.concat();
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, run.len())?;
        file.write_all(&run).map_err(|_| IoError::Failed)
    }

    fn flush(&self) -> Result<(), IoError> {
        if !self.writable {
            return Ok(());
        }
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        file.flush().map_err(|_| IoError::Failed)?;
        if self.durable {
            file.sync_data().map_err(|_| IoError::Failed)?;
        }
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.writable
    }
}

//! Write an ext2 volume with the library, for the host-side checks.
//!
//! Two consumers: `tools/mkdisk/test_parity.py` compares a `bare` image with
//! the Python formatter byte for byte, and `.github/workflows/image.yml` runs
//! `e2fsck`/`debugfs` over a `populated` and an `updated` image. Every input is
//! explicit (size, label, uuid, timestamp), so the same arguments always give
//! the same bytes.
//!
//! ```text
//! cargo run -p ext2fs --example mkimage -- OUT [--size BYTES|nK|nM|nG] [--block-size N]
//!     [--label TEXT] [--uuid 32HEX] [--now SECONDS] [--mode bare|populated|updated]
//! ```
//!
//! Built with `--features fuzz` it also runs the library's own `check::fsck`
//! over the result (for hosts without `e2fsprogs`).
//!
//! `OUT` may also come from the `EXT2FS_IMAGE_OUT` environment variable.
//!
//! * `bare`: format only (what `tools/mkdisk --no-seed` writes).
//! * `populated`: a small OS-volume-shaped tree on top (modes, owners, a manifest,
//!   a file big enough to need an indirect block, a seeded home, a sticky tmp).
//! * `updated`: `populated`, then the volume is closed, reopened and changed in
//!   place the way a rebuild changes it: a file rewritten smaller, a subtree
//!   removed, new files added, a bigger manifest.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use ext2fs::{BlockIo, Ext2, Ext2Error, Geometry, IoError, Owner, SECTOR_SIZE};

/// The clock the volume stamps new inodes with, set once from `--now`.
static NOW: AtomicI64 = AtomicI64::new(0);

fn clock() -> i64 {
    NOW.load(Ordering::Relaxed)
}

/// A volume held in a host file.
struct FileIo {
    file: Mutex<File>,
    sectors: u64,
}

impl FileIo {
    fn open(path: &str) -> std::io::Result<FileIo> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let sectors = file.metadata()?.len() / SECTOR_SIZE as u64;
        Ok(FileIo {
            file: Mutex::new(file),
            sectors,
        })
    }
}

impl BlockIo for FileIo {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(lba * SECTOR_SIZE as u64))
            .and_then(|_| file.read_exact(buf))
            .map_err(|_| IoError::Failed)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(lba * SECTOR_SIZE as u64))
            .and_then(|_| file.write_all(buf))
            .map_err(|_| IoError::Failed)
    }

    fn flush(&self) -> Result<(), IoError> {
        self.file
            .lock()
            .unwrap()
            .sync_data()
            .map_err(|_| IoError::Failed)
    }

    fn is_writable(&self) -> bool {
        true
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Bare,
    Populated,
    Updated,
}

struct Args {
    out: String,
    size: u64,
    block_size: u32,
    label: String,
    uuid: [u8; 16],
    now: i64,
    mode: Mode,
}

fn parse_uuid(text: &str) -> Result<[u8; 16], String> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(format!("--uuid needs 32 hex digits, got {text:?}"));
    }
    let mut uuid = [0u8; 16];
    for (i, byte) in uuid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| format!("--uuid is not hex: {text:?}"))?;
    }
    Ok(uuid)
}

/// A byte count with an optional `K`, `M` or `G` suffix (powers of 1024).
fn parse_number(text: &str) -> Option<u64> {
    let (digits, shift) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 10),
        'M' => (&text[..text.len() - 1], 20),
        'G' => (&text[..text.len() - 1], 30),
        _ => (text, 0),
    };
    digits.parse::<u64>().ok()?.checked_mul(1 << shift)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        out: std::env::var("EXT2FS_IMAGE_OUT").unwrap_or_default(),
        size: 8 << 20,
        block_size: 4096,
        label: "lazyos-test".into(),
        uuid: [0x5A; 16],
        now: 1_700_000_000,
        mode: Mode::Bare,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        if !arg.starts_with("--") {
            args.out = arg;
            continue;
        }
        let value = it.next().ok_or(format!("{arg} needs a value"))?;
        let number = |v: &str| parse_number(v).ok_or(format!("{arg}: not a number: {v:?}"));
        match arg.as_str() {
            "--size" => args.size = number(&value)?,
            "--block-size" => args.block_size = number(&value)? as u32,
            "--label" => args.label = value,
            "--uuid" => args.uuid = parse_uuid(&value)?,
            "--now" => args.now = number(&value)? as i64,
            "--mode" => {
                args.mode = match value.as_str() {
                    "bare" => Mode::Bare,
                    "populated" => Mode::Populated,
                    "updated" => Mode::Updated,
                    _ => return Err(format!("unknown mode {value:?}")),
                }
            }
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    if args.out.is_empty() {
        return Err("no output path (argument or EXT2FS_IMAGE_OUT)".into());
    }
    Ok(args)
}

/// Deterministic, non-repeating-looking bytes so a wrong block shows up.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn mount(path: &str) -> Result<Ext2, Ext2Error> {
    let io = FileIo::open(path).map_err(|_| Ext2Error::Io)?;
    Ext2::open(Box::new(io), clock)
}

fn populate(fs: &Ext2, now: i64) -> Result<(), Ext2Error> {
    fs.mkdir_p("/system/bin", 0o755, 0, 0)?;
    fs.write_file("/system/bin/tool", &pattern(20_000, 1), 0o755, 0, 0, now)?;
    // Large enough for the single-indirect block at every supported block size.
    fs.write_file("/system/big.bin", &pattern(300_000, 2), 0o644, 0, 0, now)?;
    fs.write_file(
        "/system/.image-manifest",
        b"f /system/bin/tool\nf /system/big.bin\n",
        0o644,
        0,
        0,
        now,
    )?;
    fs.mkdir_p("/home/user", 0o755, 1000, 1000)?;
    fs.mkdir_p("/data/tmp", 0o1777, 0, 0)?;
    Ok(())
}

/// What a rebuild does to an existing volume, minus the new image content.
fn update(fs: &Ext2, now: i64) -> Result<(), Ext2Error> {
    let later = now + 60;
    fs.write_file("/system/big.bin", &pattern(70_000, 3), 0o644, 0, 0, later)?;
    fs.remove_tree("/system/bin")?;
    fs.mkdir_p("/system/sbin", 0o755, 0, 0)?;
    fs.write_file("/system/sbin/tool2", &pattern(9_000, 4), 0o755, 0, 0, later)?;
    fs.write_file(
        "/system/.image-manifest",
        b"f /system/big.bin\nf /system/sbin/tool2\n",
        0o644,
        0,
        0,
        later,
    )?;
    // User data in a home must survive an update untouched, and may grow.
    let owner = Owner {
        uid: 1000,
        gid: 1000,
    };
    fs.create("/home/user/notes.txt", 0o600, owner)?;
    fs.write("/home/user/notes.txt", 0, b"keep me\n")?;
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    NOW.store(args.now, Ordering::Relaxed);
    let err = |what: &str| {
        let what = what.to_string();
        move |e: Ext2Error| format!("{what}: {e:?}")
    };
    let blocks =
        u32::try_from(args.size / u64::from(args.block_size)).map_err(|e| e.to_string())?;
    let file = File::create(&args.out).map_err(|e| e.to_string())?;
    file.set_len(u64::from(blocks) * u64::from(args.block_size))
        .map_err(|e| e.to_string())?;
    drop(file);
    let geometry = Geometry {
        block_size: args.block_size,
        blocks_count: blocks,
        bytes_per_inode: 16 * 1024,
    };
    let io = FileIo::open(&args.out).map_err(|e| e.to_string())?;
    ext2fs::format(&io, geometry, &args.label, args.uuid, args.now).map_err(err("format"))?;
    drop(io);
    if args.mode == Mode::Bare {
        return Ok(());
    }
    let fs = mount(&args.out).map_err(err("open"))?;
    populate(&fs, args.now).map_err(err("populate"))?;
    fs.flush().map_err(err("flush"))?;
    drop(fs);
    if args.mode == Mode::Updated {
        let fs = mount(&args.out).map_err(err("reopen"))?;
        update(&fs, args.now).map_err(err("update"))?;
        fs.flush().map_err(err("flush"))?;
    }
    Ok(())
}

/// With `--features fuzz` the library's own checker audits the result too, so
/// a host without `e2fsck` still gets a verdict (CI runs `e2fsck` regardless).
#[cfg(feature = "fuzz")]
fn self_check(path: &str) -> Result<(), String> {
    let image = std::fs::read(path).map_err(|e| e.to_string())?;
    let problems = ext2fs::check::fsck(&image);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("fsck: {}", problems.join("; ")))
    }
}

#[cfg(not(feature = "fuzz"))]
fn self_check(_path: &str) -> Result<(), String> {
    Ok(())
}

fn main() {
    let args = parse_args().unwrap_or_else(|message| {
        eprintln!("mkimage: {message}");
        std::process::exit(2);
    });
    if let Err(message) = run(&args).and_then(|()| self_check(&args.out)) {
        eprintln!("mkimage: {message}");
        std::process::exit(1);
    }
    println!("mkimage: wrote {}", args.out);
}

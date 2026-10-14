//! Make and look at FAT sticks on the host, with the same library the
//! `fatfuse` daemon runs (tools/storage and tools/fat use it):
//!
//! ```text
//! fatstick make OUT.img [--size 64M] [--label LAZYFAT] [--fat 16|32]
//!                       [--file PATH=TEXT]... [--dir PATH]...
//! fatstick ls   IMG [PATH]        # one line per entry: d|f SIZE NAME
//! fatstick cat  IMG PATH          # the file's bytes on stdout
//! fatstick info IMG               # label, FAT type, free clusters
//! ```
//!
//! `make` writes an MBR disk with one partition at 1 MiB (type 0x06 or
//! 0x0C), the way a partitioning tool leaves a stick.

use std::io::Write as _;
use std::process::exit;
use std::rc::Rc;
use std::{cell::RefCell, env, fs};

use fatfs::{format_volume, FatType, FormatVolumeOptions};
use fatvol::io::Shared;
use fatvol::mem::MemDisk;
use fatvol::parts::{candidates, Slice, Window};
use fatvol::{BlockIo, Cache, Clock, Options, Volume, SECTOR};
use fused::daemon::{FuseFs, Target};

const START: u64 = 2048;

fn die(text: &str) -> ! {
    eprintln!("fatstick: {text}");
    exit(1)
}

fn size(text: &str) -> u64 {
    let (digits, unit) = text.split_at(text.trim_end_matches(char::is_alphabetic).len());
    let value: u64 = digits.parse().unwrap_or_else(|_| die("bad --size"));
    match unit.to_ascii_uppercase().as_str() {
        "" | "B" => value,
        "K" => value << 10,
        "M" => value << 20,
        "G" => value << 30,
        _ => die("bad --size unit"),
    }
}

fn open_image(path: &str) -> (MemDisk, Window) {
    let bytes = fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let disk = MemDisk::from_image(&bytes);
    let mut dev = disk.clone();
    let window = candidates(&mut dev)
        .unwrap_or_else(|_| die("unreadable image"))
        .into_iter()
        .find(|w| {
            Volume::open(
                Slice::new(disk.clone(), *w),
                Options::default(),
                Clock::new(0),
            )
            .is_ok()
        })
        .unwrap_or_else(|| die("no FAT volume in the image"));
    (disk, window)
}

fn volume(disk: &MemDisk, window: Window, writable: bool) -> Volume<Slice<MemDisk>> {
    let options = Options {
        writable,
        ..Options::default()
    };
    Volume::open(Slice::new(disk.clone(), window), options, Clock::new(0))
        .unwrap_or_else(|e| die(&format!("open: {e:?}")))
}

fn make(args: &[String]) {
    let out = args.first().unwrap_or_else(|| die("make OUT.img [...]"));
    let (mut bytes, mut label, mut fat) = (64u64 << 20, "LAZYFAT".to_string(), 0u8);
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().unwrap_or_else(|| die("a flag needs a value"));
        match flag.as_str() {
            "--size" => bytes = size(value),
            "--label" => label = value.clone(),
            "--fat" => fat = value.parse().unwrap_or_else(|_| die("bad --fat")),
            "--file" => files.push(value.clone()),
            "--dir" => dirs.push(value.clone()),
            other => die(&format!("unknown flag {other}")),
        }
    }
    let total = bytes / SECTOR as u64;
    let part = total - START;
    let fat = if fat != 0 {
        fat
    } else if bytes >= 600 << 20 || part >= 1 << 20 {
        32
    } else if bytes >= 64 << 20 {
        32
    } else {
        16
    };
    let disk = MemDisk::new(total);
    let window = Window {
        start: START,
        sectors: part,
    };
    {
        let mut padded = [0u8; 11];
        padded.fill(b' ');
        let name = label.as_bytes();
        padded[..name.len().min(11)].copy_from_slice(&name[..name.len().min(11)]);
        let mut cache = Cache::new(Slice::new(disk.clone(), window), true);
        cache.rewind();
        let shared = Shared(Rc::new(RefCell::new(cache)));
        let kind = if fat == 32 {
            FatType::Fat32
        } else {
            FatType::Fat16
        };
        let options = FormatVolumeOptions::new()
            .fat_type(kind)
            .volume_label(padded)
            .volume_id(0x4C41_5A59);
        format_volume(&mut shared.clone(), options)
            .unwrap_or_else(|e| die(&format!("format: {e:?}")));
        shared.0.borrow_mut().flush_dirty().unwrap();
    }
    // The partition table.
    let mut mbr = [0u8; SECTOR];
    let at = 446;
    mbr[at + 4] = if fat == 32 { 0x0C } else { 0x06 };
    mbr[at + 8..at + 12].copy_from_slice(&(START as u32).to_le_bytes());
    mbr[at + 12..at + 16].copy_from_slice(&(part as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    disk.clone().write(0, &mbr).unwrap();
    let mut vol = volume(&disk, window, true);
    for dir in dirs {
        vol.mkdir(&dir, 0o755, 0, 0)
            .unwrap_or_else(|e| die(&format!("mkdir {dir}: errno {e}")));
    }
    for spec in files {
        let (name, text) = spec
            .split_once('=')
            .unwrap_or_else(|| die("--file PATH=TEXT"));
        vol.create(name, 0o644, 0, 0)
            .unwrap_or_else(|e| die(&format!("create {name}: errno {e}")));
        vol.write(Target::Path(name), 0, text.as_bytes())
            .unwrap_or_else(|e| die(&format!("write {name}: errno {e}")));
    }
    vol.settle().unwrap();
    drop(vol);
    fs::write(out, disk.image()).unwrap_or_else(|e| die(&format!("{out}: {e}")));
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("make") => make(&args[1..]),
        Some("ls") => {
            let (disk, window) = open_image(args.get(1).unwrap_or_else(|| die("ls IMG [PATH]")));
            let mut vol = volume(&disk, window, false);
            let path = args.get(2).map_or("", String::as_str);
            for entry in vol
                .readdir(path)
                .unwrap_or_else(|e| die(&format!("errno {e}")))
            {
                let full = if path.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{path}/{}", entry.name)
                };
                let size = vol.lookup(Target::Path(&full)).map_or(0, |a| a.size);
                println!(
                    "{} {size} {}",
                    if entry.dir { 'd' } else { 'f' },
                    entry.name
                );
            }
        }
        Some("cat") => {
            let (disk, window) = open_image(args.get(1).unwrap_or_else(|| die("cat IMG PATH")));
            let mut vol = volume(&disk, window, false);
            let path = args.get(2).unwrap_or_else(|| die("cat IMG PATH"));
            let size = vol
                .lookup(Target::Path(path))
                .unwrap_or_else(|e| die(&format!("{path}: errno {e}")))
                .size;
            let mut buf = vec![0u8; 64 * 1024];
            let mut at = 0;
            let mut out = std::io::stdout().lock();
            while at < size {
                let n = vol
                    .read(Target::Path(path), at, &mut buf)
                    .unwrap_or_else(|e| die(&format!("read: errno {e}")));
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).unwrap();
                at += n as u64;
            }
        }
        Some("info") => {
            let (disk, window) = open_image(args.get(1).unwrap_or_else(|| die("info IMG")));
            let mut vol = volume(&disk, window, false);
            let stats = vol.statfs().unwrap();
            println!(
                "label={} fat={} dirty={} clusters={} free={} cluster_bytes={}",
                vol.label(),
                vol.fat_bits(),
                vol.dirty_at_open(),
                stats.blocks,
                stats.blocks_free,
                stats.block_size
            );
        }
        _ => die("make | ls | cat | info (see the file's header)"),
    }
}

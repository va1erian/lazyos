//! `fatfuse` (`/system/bin/fatfuse`): a FAT volume of a USB stick, served as
//! a directory at `/media/<label>` through the FUSE mechanism
//! (docs/usb-hotplug-plan.md F, docs/architecture/usb-storage.md).
//!
//! ```text
//! fatfuse [-v] disk=<id>:<generation> owner=UID:GID [name=NAME]
//! ```
//!
//! `mountd` starts one per provider disk it finds. It takes the exclusive
//! raw claim on the disk (syscall 33), looks for a FAT volume (the whole
//! disk, or the first MBR or GPT partition whose boot sector is one), mounts
//! it with `libs/fatvol` and serves the kernel's requests until the medium
//! is pulled or it is stopped. Every file is reported as owned by `owner=`
//! (FAT has no owners); the mount is always `nosuid`, and nothing on it is
//! executable (its files have no execute bit).
//!
//! The volume is a hostile input: everything that parses it lives in
//! `libs/fatvol`, host-tested and fuzzed, and runs here, in a process whose
//! worst outcome is its own mount.
//!
//! Prints `FATFUSE:UP /media/<name> ...` once mounted, `FATFUSE:END <why>`
//! when it ends the way it is meant to (no FAT volume on the disk, the medium
//! pulled) and `FATFUSE:FAIL <reason>` otherwise, and exits with a
//! [`Failure`] code `mountd` reads (native programs have no pipe): `0` is a
//! clean stop, `12` no volume, `15` the medium was pulled while mounted (the
//! usual end of a mount).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;
use user::trace;

use fatvol::parts::{candidates, Slice, Window};
use fatvol::{BlockIo, Clock, Fault, Options, Volume, MAX_IO};
use fused::daemon::{serve_one, Buffers};
use fused::wire::{FLAG_MEDIA, FLAG_RO};
use user::sys;

/// Most disks one listing can carry (the kernel has eight slots).
const DISKS: usize = 8;
/// Longest mount name (`fused::wire::MAX_MOUNT_NAME`), leaving room for a
/// `-<n>` suffix.
const NAME_BASE_MAX: usize = 56;
/// Names tried (`label`, `label-2`, ...) before giving up.
const NAME_TRIES: u32 = 9;

/// The exit status of a daemon that stopped, by what happened. The numbers
/// are `mountd`'s contract (`libs/mounttable` `exit_reason`); 1 is a panic.
#[derive(Clone, Copy)]
enum Failure {
    Usage = 2,
    /// No such disk, or the slot holds another one now.
    Disk = 10,
    /// Another task holds the claim.
    Busy = 11,
    /// Nothing on the disk is a FAT volume.
    NoVolume = 12,
    /// A FAT volume that cannot be read.
    Damaged = 13,
    /// `/media/<name>` could not be mounted.
    Mount = 14,
    /// The medium was pulled (or the device failed for good) while mounted.
    Lost = 15,
}

type Failed = (Failure, String);

struct Options_ {
    disk: u64,
    generation: u64,
    owner: (u32, u32),
    name: Option<String>,
    verbose: bool,
}

fn usage() -> Failed {
    (
        Failure::Usage,
        String::from("usage: fatfuse [-v] disk=ID:GENERATION owner=UID:GID [name=NAME]"),
    )
}

fn pair<A: core::str::FromStr, B: core::str::FromStr>(text: &str) -> Option<(A, B)> {
    let (a, b) = text.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn parse() -> Result<Options_, Failed> {
    let mut disk = None;
    let mut owner = None;
    let mut name = None;
    let mut verbose = false;
    for arg in sys::args().skip(1) {
        if arg == "-v" {
            verbose = true;
        } else if let Some(text) = arg.strip_prefix("disk=") {
            disk = Some(pair::<u64, u64>(text).ok_or_else(usage)?);
        } else if let Some(text) = arg.strip_prefix("owner=") {
            owner = Some(pair::<u32, u32>(text).ok_or_else(usage)?);
        } else if let Some(text) = arg.strip_prefix("name=") {
            name = Some(String::from(text));
        } else {
            return Err(usage());
        }
    }
    let (disk, generation) = disk.ok_or_else(usage)?;
    Ok(Options_ {
        disk,
        generation,
        owner: owner.ok_or_else(usage)?,
        name,
        verbose,
    })
}

/// The claimed disk as a [`BlockIo`]: every request is a syscall.
#[derive(Clone, Copy)]
struct RawDisk {
    id: u64,
    sectors: u64,
    writable: bool,
}

fn fault(errno: i64) -> Fault {
    match errno {
        sys::ENODEV | sys::ESTALE => Fault::Gone,
        30 => Fault::ReadOnly,
        22 => Fault::Eof,
        _ => Fault::Io,
    }
}

impl BlockIo for RawDisk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault> {
        for (index, chunk) in buf.chunks_mut(MAX_IO).enumerate() {
            let at = lba + (index * MAX_IO / fatvol::SECTOR) as u64;
            sys::block_read(self.id, at, chunk).map_err(fault)?;
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Fault> {
        if !self.writable {
            return Err(Fault::ReadOnly);
        }
        for (index, chunk) in buf.chunks(MAX_IO).enumerate() {
            let at = lba + (index * MAX_IO / fatvol::SECTOR) as u64;
            sys::block_write(self.id, at, chunk).map_err(fault)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Fault> {
        sys::block_flush(self.id).map_err(fault)
    }
}

/// A label as a mount name: letters, digits, `-`, `_` and `.`; anything
/// else becomes `_`; never empty, never `.` or `..`.
fn mount_name(label: &str, fallback: &str) -> String {
    let mut name: String = label
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(NAME_BASE_MAX)
        .collect();
    if name.is_empty() || name == "." || name == ".." || name.chars().all(|c| c == '_') {
        name = String::from(fallback);
    }
    name
}

fn now() -> i64 {
    (sys::wall_centis() / 100) as i64
}

/// Open the first volume of the disk that is one.
fn open_volume(
    raw: RawDisk,
    owner: (u32, u32),
) -> Result<(Volume<Slice<RawDisk>>, Window), Failed> {
    let mut probe = raw;
    let windows = candidates(&mut probe).map_err(|fault| match fault {
        Fault::Gone => (Failure::Lost, String::from("the disk went away")),
        _ => (Failure::Damaged, String::from("cannot read the partition table")),
    })?;
    if windows.is_empty() {
        return Err((Failure::NoVolume, String::from("no FAT volume on the disk")));
    }
    let options = Options {
        uid: owner.0,
        gid: owner.1,
        writable: raw.writable,
        ..Options::default()
    };
    let mut worst = (Failure::NoVolume, String::from("no FAT volume on the disk"));
    for window in windows {
        let clock = Clock::new(now());
        match Volume::open(Slice::new(raw, window), options, clock) {
            Ok(volume) => return Ok((volume, window)),
            Err(fatvol::OpenError::Io(Fault::Gone)) => {
                return Err((Failure::Lost, String::from("the disk went away")));
            }
            Err(fatvol::OpenError::Io(_)) => {
                worst = (Failure::Damaged, String::from("the volume cannot be read"));
            }
            Err(fatvol::OpenError::NotFat) => {}
        }
    }
    Err(worst)
}

fn run() -> Result<(), Failed> {
    let opts = parse()?;
    let mut table = [sys::BlockDisk::default(); DISKS];
    let count = sys::block_disks(&mut table)
        .map_err(|e| (Failure::Disk, format!("cannot list the disks: errno {e}")))?;
    let info = table[..count]
        .iter()
        .find(|disk| disk.id == opts.disk && disk.generation == opts.generation)
        .copied()
        .ok_or_else(|| {
            (
                Failure::Disk,
                format!("usb{} generation {} is not there", opts.disk, opts.generation),
            )
        })?;
    sys::block_claim(opts.disk, opts.generation).map_err(|e| match e {
        sys::EBUSY => (Failure::Busy, format!("usb{} is in use", opts.disk)),
        e => (Failure::Disk, format!("claim usb{}: errno {e}", opts.disk)),
    })?;
    let raw = RawDisk {
        id: info.id,
        sectors: info.sectors,
        writable: info.writable,
    };
    let (mut volume, window) = open_volume(raw, opts.owner)?;
    if opts.verbose || volume.dirty_at_open() {
        trace::line!(
            trace::Channel::Fatfuse,
            "VOLUME usb{} start={} sectors={} fat={} label={:?} dirty={}",
            opts.disk,
            window.start,
            window.sectors,
            volume.fat_bits(),
            volume.label(),
            volume.dirty_at_open()
        );
    }
    let fallback = format!("usb{}", opts.disk);
    let base = match &opts.name {
        Some(name) => mount_name(name, &fallback),
        None => mount_name(volume.label(), &fallback),
    };
    let flags = FLAG_MEDIA | if raw.writable { 0 } else { FLAG_RO };
    let mut registered = None;
    for attempt in 1..=NAME_TRIES {
        let name = if attempt == 1 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        match sys::fuse::Mount::register(&name, flags) {
            Ok(mount) => {
                registered = Some((mount, name));
                break;
            }
            // The name is taken (another volume of the same label).
            Err(16) => continue,
            Err(e) => {
                return Err((
                    Failure::Mount,
                    format!("mount {}/{name}: errno {e}", fhs::mount::MEDIA),
                ));
            }
        }
    }
    let (mut mount, name) = registered.ok_or_else(|| {
        (
            Failure::Mount,
            format!("every name of {base} is in use"),
        )
    })?;
    trace::line!(
        trace::Channel::Fatfuse,
        "UP {}/{name} disk=usb{} fat={} {}",
        fhs::mount::MEDIA,
        opts.disk,
        volume.fat_bits(),
        if raw.writable { "rw" } else { "ro" }
    );
    let mut buffers = Buffers::new();
    let mut dirty = false;
    loop {
        volume.set_time(now());
        match serve_one(&mut volume, &mut mount, &mut buffers) {
            Ok(true) => dirty = true,
            // Idle for a wait (a second): make the volume clean, so a stick
            // pulled now is a stick left whole.
            Ok(false) => {
                if dirty {
                    dirty = false;
                    volume
                        .settle()
                        .map_err(|_| (Failure::Lost, String::from("cannot write the volume")))?;
                }
            }
            // The kernel took the mount away (it was unmounted, or its
            // provider side closed): the normal end.
            Err(_) => {
                let _ = volume.settle();
                return Ok(());
            }
        }
        if volume.failure() == Some(Fault::Gone) {
            return Err((Failure::Lost, String::from("the medium was pulled")));
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(()) => sys::exit(0),
        Err((failure, message)) => {
            // A disk with no FAT volume, or a medium that was pulled, is the
            // daemon doing its job, not failing at it.
            let normal = matches!(failure, Failure::NoVolume | Failure::Lost);
            trace::line!(
                trace::Channel::Fatfuse,
                "{} {message}",
                if normal { "END" } else { "FAIL" }
            );
            sys::exit(failure as u32)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    trace::line!(trace::Channel::Fatfuse, "FAIL panic");
    sys::exit(1)
}

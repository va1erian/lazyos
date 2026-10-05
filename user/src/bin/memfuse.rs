//! `memfuse` (`/system/bin/memfuse`): an in-memory filesystem served from
//! user space at `/mnt/<name>` (docs/smb-plan.md stage F1). It proves the
//! FUSE mechanism with no network: every `ls`, `cat`, `cp` or `echo >` under
//! the mount is a request the kernel hands this task (syscall 35) and this
//! task answers from `fused::memfs`.
//!
//! Usage: `memfuse [-r] [-s MiB] [name]` (default `mem`, 32 MiB). `-r` mounts
//! read-only. Needs `CAP_FS_PROVIDER` (root has it). It runs until it is
//! killed; the kernel then fails the mount's calls and unmounts it. Prints
//! `MEMFUSE:UP /mnt/<name>` once mounted, `MEMFUSE:FAIL <reason>` otherwise.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::panic::PanicInfo;

use fused::daemon::{serve_one, Buffers};
use fused::memfs::MemFs;
use fused::wire::FLAG_RO;
use user::sys;

const DEFAULT_NAME: &str = "mem";
const DEFAULT_MIB: usize = 32;
const MAX_MIB: usize = 512;
const MAX_NODES: usize = 4096;

struct Options {
    name: String,
    flags: u64,
    capacity: usize,
}

fn usage() -> String {
    String::from("usage: memfuse [-r] [-s MiB] [name]")
}

fn parse() -> Result<Options, String> {
    let mut opts = Options {
        name: String::from(DEFAULT_NAME),
        flags: 0,
        capacity: DEFAULT_MIB << 20,
    };
    let mut args = sys::args().skip(1);
    while let Some(arg) = args.next() {
        match arg {
            "-r" => opts.flags |= FLAG_RO,
            "-s" => {
                let mib = args.next().and_then(|v| v.parse::<usize>().ok());
                opts.capacity = mib
                    .filter(|m| (1..=MAX_MIB).contains(m))
                    .ok_or_else(usage)?
                    << 20;
            }
            name if !name.starts_with('-') => opts.name = String::from(name),
            _ => return Err(usage()),
        }
    }
    Ok(opts)
}

/// The filesystem clock: wall-clock seconds.
fn now() -> i64 {
    (sys::wall_centis() / 100) as i64
}

fn run() -> Result<(), String> {
    let opts = parse()?;
    let mut cred = sys::Cred::default();
    sys::cred_get(None, &mut cred).map_err(|e| format!("credentials: errno {e}"))?;
    let mut fs = MemFs::new(opts.capacity, MAX_NODES, cred.uid, cred.gid, now);
    let mut mount = sys::fuse::Mount::register(&opts.name, opts.flags)
        .map_err(|e| format!("mount /mnt/{}: errno {e}", opts.name))?;
    sys::write_str(&format!("MEMFUSE:UP /mnt/{}\n", opts.name));
    let mut buffers = Buffers::new();
    loop {
        serve_one(&mut fs, &mut mount, &mut buffers).map_err(|e| format!("serve: errno {e}"))?;
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(()) => sys::exit(0),
        Err(message) => {
            sys::write_str(&format!("MEMFUSE:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("MEMFUSE:FAIL panic\n");
    sys::exit(1)
}

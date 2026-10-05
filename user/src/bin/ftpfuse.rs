//! `ftpfuse` (`/system/bin/ftpfuse`): an FTP server mounted as a directory,
//! a proof of concept of a network filesystem on the FUSE mechanism
//! (docs/smb-plan.md §3; the SMB daemon will have the same shape).
//!
//! ```text
//! ftpfuse [-v] <host>[:port] [user=NAME] [pass=SECRET] [name=NAME]
//! ```
//!
//! Logs in (anonymous by default), mounts at `/mnt/<name>` (default `ftp`)
//! through syscall 35 and serves the kernel's requests until it is killed.
//! Every program then reads and writes the server's files through the
//! ordinary VFS: `ls /mnt/ftp`, `cp /tmp/x /mnt/ftp/`, `cat /mnt/ftp/y`.
//! See `ftpfuse/fs.rs` for how file operations map onto FTP commands, and
//! its limits. Passive mode, binary type, over the native socket service.
//!
//! Prints `FTPFUSE:UP /mnt/<name>` once mounted, `FTPFUSE:FAIL <reason>`
//! otherwise. `-v` echoes every FTP command and reply (never the password).

#![no_std]
#![no_main]

extern crate alloc;

// Shared with `ftp`, which uses the parts this daemon does not.
#[path = "ftpfuse/fs.rs"]
mod fs;
#[path = "ftpfuse/link.rs"]
mod link;
#[allow(dead_code)]
#[path = "ftp/session.rs"]
mod session;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::panic::PanicInfo;

use fused::daemon::{serve_one, Buffers};
use session::describe;
use user::messenger::netsock::{Addr, Client};
use user::messenger::netstack::Client as Stack;
use user::sys;

const DEFAULT_PORT: u16 = 21;
const DEFAULT_NAME: &str = "ftp";
/// Ticks to wait for `netd` at boot, and for an address.
const CONNECT_TICKS: u64 = 500;
const ADDRESS_TICKS: u64 = 1500;
/// Idle ticks between keep-alive `NOOP`s.
const KEEPALIVE_TICKS: u64 = 6000;

struct Options {
    host: String,
    port: u16,
    user: String,
    pass: String,
    name: String,
    verbose: bool,
}

fn usage() -> String {
    String::from("usage: ftpfuse [-v] <host>[:port] [user=NAME] [pass=SECRET] [name=NAME]")
}

fn parse() -> Result<Options, String> {
    let mut opts = Options {
        host: String::new(),
        port: DEFAULT_PORT,
        user: String::from("anonymous"),
        pass: String::from("lazyos@"),
        name: String::from(DEFAULT_NAME),
        verbose: false,
    };
    for arg in sys::args().skip(1) {
        if arg == "-v" {
            opts.verbose = true;
        } else if let Some(user) = arg.strip_prefix("user=") {
            opts.user = String::from(user);
        } else if let Some(pass) = arg.strip_prefix("pass=") {
            opts.pass = String::from(pass);
        } else if let Some(name) = arg.strip_prefix("name=") {
            opts.name = String::from(name);
        } else if opts.host.is_empty() && !arg.starts_with('-') {
            match arg.split_once(':') {
                Some((host, port)) => {
                    opts.host = String::from(host);
                    opts.port = port.parse().ok().filter(|p| *p != 0).ok_or_else(usage)?;
                }
                None => opts.host = String::from(arg),
            }
        } else {
            return Err(usage());
        }
    }
    if opts.host.is_empty() {
        return Err(usage());
    }
    Ok(opts)
}

/// The stack and socket services, once `netd` is up and has an address.
fn connect() -> Result<(Rc<Client>, Stack), String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    let stack = loop {
        match Stack::connect() {
            Ok(stack) => break stack,
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => sys::nap(),
        }
    };
    let deadline = sys::clock() + ADDRESS_TICKS;
    while sys::clock() < deadline && !stack.addresses().is_ok_and(|a| !a.is_empty()) {
        sys::nap();
    }
    let sockets = Client::connect().map_err(|e| format!("sockets: {}", describe(&e)))?;
    Ok((Rc::new(sockets), stack))
}

fn run() -> Result<(), String> {
    let opts = parse()?;
    let (sockets, stack) = connect()?;
    let ip = stack
        .lookup_host(&opts.host, 5000)
        .map_err(|e| format!("{}: cannot resolve ({})", opts.host, describe(&e)))?;
    let mut link = link::Link::new(
        sockets,
        Addr::new(ip, opts.port),
        opts.user,
        opts.pass,
        opts.verbose,
    );
    link.connect().map_err(|e| format!("{}: {e}", opts.host))?;
    let mut cred = sys::Cred::default();
    sys::cred_get(None, &mut cred).map_err(|e| format!("credentials: errno {e}"))?;
    let started = (sys::wall_centis() / 100) as i64;
    let mut tree = fs::FtpFs::new(link, cred.uid, cred.gid, started);
    let mut mount = sys::fuse::Mount::register(&opts.name, 0)
        .map_err(|e| format!("mount /mnt/{}: errno {e}", opts.name))?;
    sys::write_str(&format!("FTPFUSE:UP /mnt/{}\n", opts.name));
    let mut buffers = Buffers::new();
    let mut idle_since = sys::clock();
    loop {
        let served = serve_one(&mut tree, &mut mount, &mut buffers)
            .map_err(|e| format!("serve: errno {e}"))?;
        if served {
            idle_since = sys::clock();
        } else if sys::clock() >= idle_since + KEEPALIVE_TICKS {
            tree.keepalive();
            idle_since = sys::clock();
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(()) => sys::exit(0),
        Err(message) => {
            sys::write_str(&format!("FTPFUSE:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("FTPFUSE:FAIL panic\n");
    sys::exit(1)
}

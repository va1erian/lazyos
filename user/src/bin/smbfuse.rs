//! `smbfuse` (`/system/bin/smbfuse`): an SMB 2.1 share mounted as a
//! directory (docs/smb-plan.md §4.4, stage F3).
//!
//! ```text
//! smbfuse [-f] [-v] [--sign | --sign-required | --no-sign] [-p PORT] [-W DOMAIN]
//!         -U USER //server/share [name=NAME] [owner=UID:GID]
//! ```
//!
//! Logs on (NTLMv2, `libs/smbwire`), connects the share, mounts it at
//! `/mnt/<name>` (default: the share's name in lower case) through syscall 35
//! and serves the kernel's requests until it is killed. Every program then
//! reads and writes the share's files through the ordinary VFS: `ls
//! /mnt/share`, `cp /tmp/x /mnt/share/`, `cat /mnt/share/y`. How each
//! operation maps onto SMB2, the caches and reconnects are `libs/smbfs`.
//!
//! The password is never an argument (docs/smb-plan.md §6): it comes from
//! `LAZYOS_SMB_PASSWORD` (how `mountd` hands it over), and the program then
//! serves in place. Without it, `smbfuse` asks for it at a prompt that does
//! not echo, starts itself again in the background with the password in
//! that copy's environment, and returns once the share is mounted (status
//! 0) or the daemon failed (its status), like `mount.cifs`; `-f` serves in
//! the foreground instead. The password stays in the daemon's memory, so a
//! lost session can log on again.
//!
//! `owner=` is the uid and gid the files are reported as owned by (default:
//! the daemon's own); `mountd` passes the requester's. Serving never touches
//! the filesystem (`fs/fuse/mod.rs`'s rule: the mount table is held while a
//! path request waits on the daemon): only the socket service and the
//! console.
//!
//! Prints `SMBFUSE:UP /mnt/<name> dialect=0x0210 signing=on|off` once
//! mounted, `SMBFUSE:RECONNECT n=<count>` after a lost session is replaced,
//! `SMBFUSE:FAIL <reason>` otherwise, and then exits with a [`Failure`]
//! code naming the step that failed (`mountd` reads the code, not the line).

#![no_std]
#![no_main]

extern crate alloc;

// Shared with `smb`, which uses the parts this daemon does not.
#[allow(dead_code)]
#[path = "smb/link.rs"]
mod link;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use fused::daemon::{serve_one, Buffers};
use smbfs::{Connect, Options, SmbFs};
use smbwire::client::{Client, Signing};
use smbwire::Error;
use user::sys::{self, Personality, SpawnCred};

use link::{explain, Step, Tcp};

const DEFAULT_PORT: u16 = 445;
/// The environment variable the password arrives in.
const PASSWORD_VAR: &str = "LAZYOS_SMB_PASSWORD";
/// Ticks the foreground command waits for its daemon to mount: longer than
/// the daemon's own waits for the network (20 s) and a reply (30 s).
const DETACH_TICKS: u64 = 6000;

/// The exit status of a daemon that stopped, by the step that failed: the
/// numbers are `mountd`'s contract (`libs/mounttable` `exit_reason`), shared
/// with `ftpfuse`; 1 is a panic.
#[derive(Clone, Copy)]
enum Failure {
    Usage = 2,
    Network = 3,
    Resolve = 4,
    Login = 5,
    Mount = 6,
    Serve = 7,
    Share = 8,
}

type Failed = (Failure, String);

struct Args {
    server: String,
    share: String,
    port: u16,
    user: String,
    domain: Option<String>,
    signing: Signing,
    name: Option<String>,
    owner: Option<(u32, u32)>,
    verbose: bool,
    foreground: bool,
}

fn usage() -> Failed {
    let text = "usage: smbfuse [-f] [-v] [--sign|--sign-required|--no-sign] [-p PORT] [-W DOMAIN] \
                -U USER //server/share [name=NAME] [owner=UID:GID]";
    (Failure::Usage, String::from(text))
}

/// `//server/share` (or `\\server\share`).
fn target(word: &str) -> Option<(String, String)> {
    let rest = word
        .strip_prefix("//")
        .or_else(|| word.strip_prefix("\\\\"))?;
    let (server, share) = rest.split_once(['/', '\\'])?;
    let share = share.trim_end_matches(['/', '\\']);
    (!server.is_empty() && smbwire::name::share_ok(share))
        .then(|| (String::from(server), String::from(share)))
}

fn owner(text: &str) -> Option<(u32, u32)> {
    let (uid, gid) = text.split_once(':')?;
    Some((uid.parse().ok()?, gid.parse().ok()?))
}

fn parse() -> Result<Args, Failed> {
    let mut opts = Args {
        server: String::new(),
        share: String::new(),
        port: DEFAULT_PORT,
        user: String::new(),
        domain: None,
        signing: Signing::Auto,
        name: None,
        owner: None,
        verbose: false,
        foreground: false,
    };
    let mut args = sys::args().skip(1);
    while let Some(arg) = args.next() {
        match arg {
            "-v" => opts.verbose = true,
            "-f" => opts.foreground = true,
            "--sign" => opts.signing = Signing::Always,
            "--sign-required" => opts.signing = Signing::Required,
            "--no-sign" => opts.signing = Signing::Never,
            "-p" => {
                let port = args.next().and_then(|p| p.parse().ok()).filter(|p| *p != 0);
                opts.port = port.ok_or_else(usage)?;
            }
            "-W" => opts.domain = Some(String::from(args.next().ok_or_else(usage)?)),
            "-U" => opts.user = String::from(args.next().ok_or_else(usage)?),
            other => {
                if let Some(name) = other.strip_prefix("name=") {
                    opts.name = Some(String::from(name));
                } else if let Some(text) = other.strip_prefix("owner=") {
                    opts.owner = Some(owner(text).ok_or_else(usage)?);
                } else if opts.server.is_empty() {
                    (opts.server, opts.share) = target(other).ok_or_else(usage)?;
                } else {
                    return Err(usage());
                }
            }
        }
    }
    if opts.server.is_empty() || opts.user.is_empty() {
        return Err(usage());
    }
    Ok(opts)
}

/// The mount name: `name=`, else the share's name when it is one.
fn mount_name(opts: &Args) -> String {
    let share = opts.share.to_ascii_lowercase();
    match &opts.name {
        Some(name) => name.clone(),
        None if fused::wire::valid_mount_name(share.as_bytes()) => share,
        None => String::from("smb"),
    }
}

/// The password at a prompt that does not echo.
fn prompt(opts: &Args) -> String {
    sys::write_str(&format!("Password for {}@{}: ", opts.user, opts.server));
    let mut secret = String::new();
    loop {
        match sys::read_char() as u8 {
            0 | b'\n' | b'\r' => break,
            8 => {
                secret.pop();
            }
            byte if byte >= 0x20 && secret.len() < 512 => secret.push(char::from(byte)),
            _ => {}
        }
    }
    sys::write_str("\n");
    secret
}

/// Logs on again after a lost session: the same server, share and
/// credential (a session is only ever reused for that identity, §4.4).
struct Dialer {
    opts: Args,
    secret: String,
}

impl Dialer {
    fn target(&self) -> link::Target<'_> {
        link::Target {
            server: &self.opts.server,
            port: self.opts.port,
            user: &self.opts.user,
            password: &self.secret,
            domain: self.opts.domain.as_deref(),
            signing: self.opts.signing,
        }
    }
}

impl Connect<Tcp> for Dialer {
    fn connect(&mut self) -> Result<Client<Tcp>, Error> {
        let (mut client, _) = link::open_step(&self.target()).map_err(|(_, why)| {
            if self.opts.verbose {
                sys::write_str(&format!("smbfuse: reconnect: {why}\n"));
            }
            Error::Closed
        })?;
        client.tree_connect(&self.opts.server, &self.opts.share)?;
        Ok(client)
    }
}

/// The files' owner: `owner=` when given, else this daemon's identity.
fn files_owner(opts: &Args) -> Result<(u32, u32), Failed> {
    if let Some(owner) = opts.owner {
        return Ok(owner);
    }
    let cred =
        sys::cred_get(None).map_err(|e| (Failure::Mount, format!("credentials: errno {e}")))?;
    Ok((cred.uid, cred.gid))
}

/// Start this program again as the daemon, the password in its environment
/// (never its arguments), and return once its mount point answers. A daemon
/// that fails first has printed why: this command ends with its status.
fn detach(point: &str, secret: &str) -> Result<(), Failed> {
    // Something already answers there: its appearing would not prove that
    // this command mounted anything.
    if user::files::stat(point).is_ok() {
        return Err((Failure::Mount, format!("{point} is already mounted")));
    }
    let argv: Vec<&str> = sys::args().collect();
    let env = format!("{PASSWORD_VAR}={secret}");
    let spawned = sys::spawnv(
        fhs::bin::SMBFUSE,
        &argv,
        &[env.as_str()],
        Personality::Native,
        SpawnCred::Inherit,
    );
    env.into_bytes().fill(0);
    let pid = spawned.map_err(|e| {
        (
            Failure::Mount,
            format!("cannot start the daemon: errno {e}"),
        )
    })?;
    let deadline = sys::clock() + DETACH_TICKS;
    loop {
        if let Some((child, status)) = sys::wait(sys::clock() + 10) {
            if child == pid {
                sys::exit(status as u32);
            }
        }
        if user::files::stat(point).is_ok() {
            return Ok(());
        }
        if sys::clock() >= deadline {
            let _ = sys::kill(pid, sys::SIG_KILL);
            return Err((
                Failure::Login,
                String::from("the daemon did not mount in time"),
            ));
        }
    }
}

fn run() -> Result<(), Failed> {
    let opts = parse()?;
    let name = mount_name(&opts);
    let point = format!("{}/{}", fhs::mount::MNT, name);
    let (uid, gid) = files_owner(&opts)?;
    let secret = match sys::getenv(PASSWORD_VAR) {
        Some(secret) => String::from(secret),
        None if opts.foreground => prompt(&opts),
        None => return detach(&point, &prompt(&opts)),
    };
    let dialer = Dialer { opts, secret };
    let (mut client, logon) = link::open_step(&dialer.target()).map_err(|(step, why)| {
        let failure = match step {
            Step::Network => Failure::Network,
            Step::Resolve => Failure::Resolve,
            Step::Login => Failure::Login,
        };
        (failure, why)
    })?;
    client
        .tree_connect(&dialer.opts.server, &dialer.opts.share)
        .map_err(|e| (Failure::Share, explain(&e)))?;
    let mut mount = sys::fuse::Mount::register(&name, 0)
        .map_err(|e| (Failure::Mount, format!("mount {point}: errno {e}")))?;
    sys::write_str(&format!(
        "SMBFUSE:UP {point} dialect={:#06x} signing={}\n",
        logon.dialect,
        if logon.signing { "on" } else { "off" },
    ));
    let options = Options {
        uid,
        gid,
        started: (sys::wall_centis() / 100) as i64,
        clock: sys::clock,
    };
    let mut tree = SmbFs::new(client, dialer, options);
    let mut buffers = Buffers::new();
    let mut reconnects = 0;
    loop {
        let served = serve_one(&mut tree, &mut mount, &mut buffers)
            .map_err(|e| (Failure::Serve, format!("serve: errno {e}")))?;
        if !served {
            tree.idle();
        }
        if tree.stats().reconnects != reconnects {
            reconnects = tree.stats().reconnects;
            sys::write_str(&format!("SMBFUSE:RECONNECT n={reconnects}\n"));
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(()) => sys::exit(0),
        Err((failure, message)) => {
            sys::write_str(&format!("SMBFUSE:FAIL {message}\n"));
            sys::exit(failure as u32)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("SMBFUSE:FAIL panic\n");
    sys::exit(1)
}

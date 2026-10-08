//! `smb` (`/system/bin/smb`): an SMB 2.1 client for file transfer and
//! diagnostics (docs/smb-plan.md §4.5, stage F2).
//!
//! ```text
//! smb [-q] [-v] [--sign | --sign-required | --no-sign] [-p PORT] [-W DOMAIN]
//!     -U USER //server/share [cmd ; cmd ...]
//! ```
//!
//! The password is never an argument: it comes from `LAZYOS_SMB_PASSWORD` or,
//! without it, a prompt that does not echo. Commands (separated by `;`; with
//! none, a prompt reads them one line at a time): `ls [path]`, `cd <dir>`,
//! `pwd`, `get <remote> [-|!]`, `put <local> [remote]`, `put -g <bytes>
//! <remote>`, `mkdir <dir>`, `rm <file>`, `rmdir <dir>`, `mv <from> <to>`,
//! `df`, `quit`.
//!
//! **Files.** Native programs have no file-write syscall, so `get` cannot name
//! a local file: `get <remote> -` writes the bytes to standard output and
//! `get <remote> !` throws them away after checksumming them. `put <local>`
//! reads a file with the native `read_file`; `put -g <n>` sends `n`
//! deterministic bytes (the stream `nc -g` and `ftp` use). Moving files both
//! ways through a directory is F3's mount.
//!
//! **Markers** for the harness: `SMB:DIALECT 0x0210`, `SMB:LOGON user=..
//! domain=.. signing=on|off spnego=yes|no`, `SMB:TREE share=..`, `SMB:LIST
//! n=..`, `SMB:GET <name> bytes=N crc=XXXXXXXX`, `SMB:PUT <name> bytes=N
//! crc=XXXXXXXX`, then `SMB:PASS commands=N` or `SMB:FAIL reason=..`. They say
//! when; the verdict is the server's record and the capture.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "smb/cmds.rs"]
mod cmds;
#[path = "smb/link.rs"]
mod link;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use smbwire::client::Signing;
use user::sys;

const DEFAULT_PORT: u16 = 445;
/// The environment variable a headless run passes the password in.
const PASSWORD_VAR: &str = "LAZYOS_SMB_PASSWORD";

pub struct Options {
    server: String,
    share: String,
    port: u16,
    user: String,
    domain: Option<String>,
    signing: Signing,
    quiet: bool,
    verbose: bool,
    script: Vec<Vec<String>>,
}

fn usage() -> String {
    String::from(
        "usage: smb [-q] [-v] [--sign|--sign-required|--no-sign] [-p PORT] [-W DOMAIN] \
         -U USER //server/share [cmd ; cmd ...]",
    )
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

fn parse() -> Result<Options, String> {
    let mut buffer = [0u8; 1024];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let line = core::str::from_utf8(&buffer[..len]).map_err(|_| usage())?;
    let mut words = line.split_whitespace();
    let mut opts = Options {
        server: String::new(),
        share: String::new(),
        port: DEFAULT_PORT,
        user: String::new(),
        domain: None,
        signing: Signing::Auto,
        quiet: false,
        verbose: false,
        script: Vec::new(),
    };
    while let Some(word) = words.next() {
        match word {
            "-q" => opts.quiet = true,
            "-v" => opts.verbose = true,
            "--sign" => opts.signing = Signing::Always,
            "--sign-required" => opts.signing = Signing::Required,
            "--no-sign" => opts.signing = Signing::Never,
            "-p" => {
                let port = words
                    .next()
                    .and_then(|p| p.parse().ok())
                    .filter(|p| *p != 0);
                opts.port = port.ok_or_else(usage)?;
            }
            "-W" => opts.domain = Some(String::from(words.next().ok_or_else(usage)?)),
            "-U" => opts.user = String::from(words.next().ok_or_else(usage)?),
            other => {
                let (server, share) = target(other).ok_or_else(usage)?;
                opts.server = server;
                opts.share = share;
                break;
            }
        }
    }
    if opts.server.is_empty() || opts.user.is_empty() {
        return Err(usage());
    }
    let mut current: Vec<String> = Vec::new();
    for word in words {
        if word == ";" {
            if !current.is_empty() {
                opts.script.push(core::mem::take(&mut current));
            }
        } else {
            current.push(String::from(word));
        }
    }
    if !current.is_empty() {
        opts.script.push(current);
    }
    Ok(opts)
}

/// One line from the keyboard or a redirected stdin, echoed or not.
pub fn read_line(echo: bool) -> String {
    let mut line = String::new();
    loop {
        match sys::read_char() as u8 {
            0 | b'\n' | b'\r' => break,
            8 => {
                if line.pop().is_some() && echo {
                    sys::write_str("\x08 \x08");
                }
            }
            byte if byte >= 0x20 && line.len() < 512 => {
                line.push(char::from(byte));
                if echo {
                    sys::write(&[byte]);
                }
            }
            _ => {}
        }
    }
    if echo {
        sys::write_str("\n");
    }
    line
}

/// The password: the environment for a headless run, else a silent prompt.
fn password(opts: &Options) -> String {
    if let Some(secret) = sys::getenv(PASSWORD_VAR) {
        return String::from(secret);
    }
    sys::write_str(&format!("Password for {}@{}: ", opts.user, opts.server));
    let secret = read_line(false);
    sys::write_str("\n");
    secret
}

fn run() -> Result<usize, String> {
    let opts = parse()?;
    let secret = password(&opts);
    let target = link::Target {
        server: &opts.server,
        port: opts.port,
        user: &opts.user,
        password: &secret,
        domain: opts.domain.as_deref(),
        signing: opts.signing,
    };
    let (mut client, logon) = link::open(&target)?;
    // The password is no longer needed: overwrite it rather than leave it
    // in a freed block of the heap.
    secret.into_bytes().fill(0);
    sys::write_str(&format!("SMB:DIALECT {:#06x}\n", logon.dialect));
    sys::write_str(&format!(
        "SMB:LOGON user={} domain={} signing={} spnego={}\n",
        opts.user,
        logon.domain,
        if logon.signing { "on" } else { "off" },
        if logon.spnego { "yes" } else { "no" },
    ));
    client
        .tree_connect(&opts.server, &opts.share)
        .map_err(|e| link::explain(&e))?;
    sys::write_str(&format!("SMB:TREE share={}\n", opts.share));
    let mut session = cmds::Session::new(client, opts.quiet, opts.verbose);
    let done = session.run_all(&opts.script)?;
    session.close();
    Ok(done)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(done) => {
            sys::write_str(&format!("SMB:PASS commands={done}\n"));
            sys::exit(0)
        }
        Err(message) => {
            sys::write_str(&format!("SMB:FAIL reason={message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}

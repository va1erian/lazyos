//! `ftp` (`/system/bin/ftp`): a passive-mode FTP client (`docs/networking-plan.md`,
//! stage N4).
//!
//! ```text
//! ftp [-q] <host>[:port] [user=NAME] [pass=SECRET] [command ; command ...]
//! ```
//!
//! Commands (separated by `;`; with none, a prompt reads them one line at a
//! time): `pwd`, `cd <dir>`, `ls [path]`, `size <file>`, `get <remote> [-|!]`,
//! `put <local> [remote]`, `put -g <bytes> <remote>`, `quit`.
//!
//! **Files.** Native programs have no file-write syscall, so `get` cannot name a
//! local file: `get <remote> -` writes the bytes to standard output (redirect it:
//! `ftp -q host get x - > /tmp/x`), and `get <remote> !` throws them away after
//! counting and checksumming them. `put <local>` reads a file with the native
//! `read_file`; `put -g <n> <remote>` sends `n` deterministic bytes (the stream
//! `nc -g` uses). Binary (`TYPE I`) always, passive (`PASV`) only.
//!
//! **Markers** for the harness: `FTP:LOGIN`, `FTP:GET <name> bytes=N crc=XXXXXXXX`,
//! `FTP:PUT <name> bytes=N crc=XXXXXXXX`, `FTP:LS bytes=N`, then `FTP:PASS
//! commands=N` or `FTP:FAIL <why>`. The host judges the bytes from the capture
//! and its own server, never from these.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "ftp/session.rs"]
mod session;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use ftpwire::{crc32_update, Pattern};
use session::{describe, drain, send_all, Control};
use user::messenger::netsock::{Addr, Client};
use user::messenger::netstack::Client as Stack;
use user::sys;

/// Ticks to wait for `netd` at boot, and for an address.
const CONNECT_TICKS: u64 = 500;
const ADDRESS_TICKS: u64 = 1500;
/// Largest file `put` reads, and `put -g` generates.
const MAX_FILE: usize = 4 * 1024 * 1024;
/// Largest listing printed.
const MAX_LISTING: usize = 1024 * 1024;
const DEFAULT_PORT: u16 = 21;

struct Options {
    host: String,
    port: u16,
    user: String,
    pass: String,
    quiet: bool,
    script: Vec<Vec<String>>,
}

fn usage() -> String {
    String::from("usage: ftp [-q] <host>[:port] [user=NAME] [pass=SECRET] [cmd ; cmd ...]")
}

fn parse() -> Result<Options, String> {
    let mut buffer = [0u8; 1024];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let line = core::str::from_utf8(&buffer[..len]).map_err(|_| usage())?;
    let mut words = line.split_whitespace().peekable();
    let mut opts = Options {
        host: String::new(),
        port: DEFAULT_PORT,
        user: String::from("anonymous"),
        pass: String::from("lazyos@"),
        quiet: false,
        script: Vec::new(),
    };
    if words.peek() == Some(&"-q") {
        opts.quiet = true;
        words.next();
    }
    let target = words.next().ok_or_else(usage)?;
    match target.split_once(':') {
        Some((host, port)) => {
            opts.host = String::from(host);
            opts.port = port.parse().ok().filter(|p| *p != 0).ok_or_else(usage)?;
        }
        None => opts.host = String::from(target),
    }
    while let Some(word) = words.peek().copied() {
        if let Some(name) = word.strip_prefix("user=") {
            opts.user = String::from(name);
        } else if let Some(secret) = word.strip_prefix("pass=") {
            opts.pass = String::from(secret);
        } else {
            break;
        }
        words.next();
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

fn connect() -> Result<(Rc<Client>, Stack), String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    let stack = loop {
        match Stack::connect() {
            Ok(stack) => break stack,
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => {
                sys::nap();
            }
        }
    };
    let deadline = sys::clock() + ADDRESS_TICKS;
    while sys::clock() < deadline && !stack.addresses().is_ok_and(|a| !a.is_empty()) {
        sys::nap();
    }
    let sockets = Client::connect().map_err(|e| format!("sockets: {}", describe(&e)))?;
    Ok((Rc::new(sockets), stack))
}

fn cmd_ls(c: &mut Control, args: &[String]) -> Result<(), String> {
    let data = c.transfer("LIST", args.first().map(String::as_str))?;
    // A listing is text from the server: control characters (escape sequences
    // that could rewrite the terminal) are shown as `?`; only line and tab
    // characters pass.
    let bytes = drain(&data, MAX_LISTING, |chunk| {
        let safe: Vec<u8> = chunk
            .iter()
            .map(|&b| {
                if matches!(b, 10 | 13 | 9 | 0x20..=0x7E) {
                    b
                } else {
                    b'?'
                }
            })
            .collect();
        sys::write(&safe);
    })?;
    drop(data);
    c.finish_transfer()?;
    sys::write_str(&format!("FTP:LS bytes={bytes}\n"));
    Ok(())
}

fn cmd_get(c: &mut Control, args: &[String], quiet: bool) -> Result<(), String> {
    let name = args.first().ok_or("get: which file?")?;
    let to_stdout = args.get(1).map(String::as_str) == Some("-");
    let data = c.transfer("RETR", Some(name))?;
    let mut crc = 0u32;
    let bytes = drain(&data, usize::MAX, |chunk| {
        crc = crc32_update(crc, chunk);
        if to_stdout {
            sys::write(chunk);
        }
    })?;
    drop(data);
    c.finish_transfer()?;
    if !(to_stdout && quiet) {
        sys::write_str(&format!("FTP:GET {name} bytes={bytes} crc={crc:08x}\n"));
    }
    Ok(())
}

/// What `put` sends: a file read whole, or a generated stream.
enum Source {
    File(Vec<u8>),
    Generated(usize),
}

fn cmd_put(c: &mut Control, args: &[String]) -> Result<(), String> {
    let (source, remote) = match args {
        [flag, n, remote] if flag == "-g" => {
            let n: usize = n.parse().map_err(|_| "put -g: bytes?")?;
            (Source::Generated(n.clamp(1, MAX_FILE)), remote.as_str())
        }
        [local, rest @ ..] => {
            let remote = rest.first().map_or(local.as_str(), String::as_str);
            let mut path = Vec::from(local.as_bytes());
            path.push(0);
            let mut buffer = alloc::vec![0u8; MAX_FILE + 1];
            let n = sys::read_file(&path, &mut buffer)
                .ok_or_else(|| format!("put: no file {local}"))?;
            if n > MAX_FILE {
                return Err(format!("put: {local} is over {MAX_FILE} bytes"));
            }
            buffer.truncate(n);
            (Source::File(buffer), remote)
        }
        [] => return Err(String::from("put: which file?")),
    };
    let data = c.transfer("STOR", Some(remote))?;
    let (mut crc, mut sent) = (0u32, 0usize);
    let mut pattern = Pattern::new();
    let total = match &source {
        Source::File(bytes) => bytes.len(),
        Source::Generated(n) => *n,
    };
    let mut chunk = alloc::vec![0u8; 8192];
    while sent < total {
        let n = (total - sent).min(chunk.len());
        match &source {
            Source::File(bytes) => chunk[..n].copy_from_slice(&bytes[sent..sent + n]),
            Source::Generated(_) => pattern.fill(&mut chunk[..n]),
        }
        send_all(&data, &chunk[..n])?;
        crc = crc32_update(crc, &chunk[..n]);
        sent += n;
    }
    // End of file for the server is the end of the data connection.
    let _ = data.shutdown_write();
    drop(data);
    c.finish_transfer()?;
    sys::write_str(&format!("FTP:PUT {remote} bytes={sent} crc={crc:08x}\n"));
    Ok(())
}

/// Run one command. `Ok(false)` means `quit`.
fn run_command(c: &mut Control, words: &[String], quiet: bool) -> Result<bool, String> {
    let Some((verb, args)) = words.split_first() else {
        return Ok(true);
    };
    match verb.as_str() {
        "quit" | "bye" | "exit" => return Ok(false),
        "pwd" => {
            let r = c.request("PWD", None, &[2])?;
            sys::write_str(&format!("{}\n", r.text()));
        }
        "cd" => {
            c.request("CWD", Some(args.first().ok_or("cd: where?")?), &[2])?;
        }
        "size" => {
            let name = args.first().ok_or("size: which file?")?;
            let r = c.request("SIZE", Some(name), &[2])?;
            sys::write_str(&format!("{name}: {}\n", r.text()));
        }
        "ls" | "dir" => cmd_ls(c, args)?,
        "get" => cmd_get(c, args, quiet)?,
        "put" => cmd_put(c, args)?,
        other => return Err(format!("unknown command {other:?}")),
    }
    Ok(true)
}

/// One line from the keyboard or a redirected stdin; `None` on an empty line.
fn read_line() -> Option<String> {
    let mut line = String::new();
    loop {
        match sys::read_char() as u8 {
            b'\n' | b'\r' => break,
            byte if byte >= 0x20 && line.len() < 512 => line.push(char::from(byte)),
            _ => {}
        }
    }
    (!line.is_empty()).then_some(line)
}

fn run() -> Result<usize, String> {
    let opts = parse()?;
    let (sockets, stack) = connect()?;
    let ip = stack
        .lookup_host(&opts.host, 5000)
        .map_err(|e| format!("{}: cannot resolve ({})", opts.host, describe(&e)))?;
    let mut c = Control::open(&sockets, Addr::new(ip, opts.port), !opts.quiet)?;
    let greeting = c.reply()?;
    if greeting.class() != 2 {
        return Err(format!("greeting: {} {}", greeting.code, greeting.text()));
    }
    let r = c.request("USER", Some(&opts.user), &[2, 3])?;
    if r.class() == 3 {
        c.request("PASS", Some(&opts.pass), &[2])?;
    }
    sys::write_str("FTP:LOGIN\n");
    c.request("TYPE", Some("I"), &[2])?;

    let mut done = 0;
    if opts.script.is_empty() {
        loop {
            sys::write_str("ftp> ");
            let Some(line) = read_line() else { break };
            let words: Vec<String> = line.split_whitespace().map(String::from).collect();
            match run_command(&mut c, &words, opts.quiet) {
                Ok(true) => done += 1,
                Ok(false) => break,
                // An interactive session survives a failed command.
                Err(message) => sys::write_str(&format!("? {message}\n")),
            }
        }
    } else {
        for words in &opts.script {
            if !run_command(&mut c, words, opts.quiet)? {
                break;
            }
            done += 1;
        }
    }
    c.quit();
    Ok(done)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match run() {
        Ok(done) => {
            sys::write_str(&format!("FTP:PASS commands={done}\n"));
            sys::exit(0)
        }
        Err(message) => {
            sys::write_str(&format!("FTP:FAIL {message}\n"));
            sys::exit(1)
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}

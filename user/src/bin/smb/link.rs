//! The connection under `smb` (and, in F3, `smbfuse`): `libs/smbwire`'s
//! [`Transport`] over a native TCP stream, the logon, and how a failure reads.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use smbwire::client::{Client, Config, Logon, Signing, Transport};
use smbwire::header::command;
use smbwire::{status, Error};
use user::messenger::netsock::{errno, Addr, Client as Sockets};
use user::messenger::netstack::Client as Stack;
use user::messenger::netstd::{is_timeout, TcpStream};
use user::messenger::Error as MsgError;
use user::sys;

/// Milliseconds to wait for the connection to open.
const CONNECT_MS: u32 = 8000;
/// Milliseconds a response may take.
const REPLY_MS: u64 = 30_000;
/// Milliseconds one read waits before the clock is checked.
const SLICE_MS: u32 = 200;
/// Ticks to wait for `netd` at boot, and for an address.
const STACK_TICKS: u64 = 500;
const ADDRESS_TICKS: u64 = 1500;
/// Seconds between 1601 (FILETIME) and 1970 (Unix).
const FILETIME_UNIX: u64 = 11_644_473_600;
/// 2026-01-02 00:00 UTC: a wall clock before this is the kernel's fallback,
/// not a real time (docs/smb-plan.md §5).
const PLAUSIBLE_UNIX: u64 = 1_767_312_000;

pub struct Tcp {
    stream: TcpStream,
}

impl Transport for Tcp {
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.stream
            .write_all(bytes, REPLY_MS as u32)
            .map_err(|e| Error::Transport(describe(&e)))
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        let deadline = sys::clock() + REPLY_MS / 10;
        loop {
            match self.stream.read(16 * 1024, SLICE_MS) {
                Ok(bytes) => return Ok(bytes),
                Err(e) if is_timeout(&e) && sys::clock() < deadline => {}
                Err(e) if is_timeout(&e) => {
                    return Err(Error::Transport(String::from(
                        "no response from the server",
                    )))
                }
                Err(e) => return Err(Error::Transport(describe(&e))),
            }
        }
    }
}

pub fn describe(error: &MsgError) -> String {
    match error {
        MsgError::Errno(code) if *code == -errno::ECONNREFUSED => {
            String::from("connection refused")
        }
        MsgError::Errno(code) if *code == -errno::ENETUNREACH => {
            String::from("network is unreachable")
        }
        MsgError::Errno(code) if *code == -errno::ECONNRESET => String::from("connection reset"),
        MsgError::Errno(code) if *code == -user::messenger::errno::ETIMEDOUT => {
            String::from("timed out")
        }
        other => String::from(other.message()),
    }
}

/// The stack and an address, waiting for `netd` at boot.
fn sockets() -> Result<(Rc<Sockets>, Stack), String> {
    let deadline = sys::clock() + STACK_TICKS;
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
    let sockets = Sockets::connect().map_err(|e| format!("sockets: {}", describe(&e)))?;
    Ok((Rc::new(sockets), stack))
}

/// What a logon needs from the command line.
pub struct Target<'a> {
    pub server: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub password: &'a str,
    pub domain: Option<&'a str>,
    pub signing: Signing,
}

/// The client's clock as FILETIME, and whether it looks real.
fn filetime() -> (u64, bool) {
    let unix = sys::wall_centis() / 100;
    ((unix + FILETIME_UNIX) * 10_000_000, unix >= PLAUSIBLE_UNIX)
}

/// Resolve, connect and log on.
pub fn open(target: &Target) -> Result<(Client<Tcp>, Logon), String> {
    let (sockets, stack) = sockets()?;
    let ip = stack
        .lookup_host(target.server, 5000)
        .map_err(|e| format!("{}: cannot resolve ({})", target.server, describe(&e)))?;
    let stream = TcpStream::connect(&sockets, Addr::new(ip, target.port), CONNECT_MS)
        .map_err(|e| format!("connect: {}", describe(&e)))?;
    let mut random = [0u8; 24];
    sys::random(&mut random).map_err(|code| format!("no randomness ({code})"))?;
    let (time, plausible) = filetime();
    let cfg = Config {
        user: target.user,
        password: target.password,
        domain: target.domain,
        workstation: "LAZYOS",
        signing: target.signing,
        client_guid: random[..16].try_into().unwrap_or([0; 16]),
        client_challenge: random[16..].try_into().unwrap_or([0; 8]),
        time,
    };
    Client::connect(Tcp { stream }, &cfg).map_err(|error| {
        let mut text = explain(&error);
        if error.status() == Some(status::LOGON_FAILURE) && !plausible {
            text.push_str("; the clock has no real time, which NTLMv2 may refuse");
        }
        text
    })
}

/// A failure in words, naming the status for a server refusal.
pub fn explain(error: &Error) -> String {
    match error {
        Error::Status {
            command: c,
            status: s,
        } => {
            let what = match (*c, *s) {
                (_, status::LOGON_FAILURE) => " (wrong user, password or domain)",
                (_, status::BAD_NETWORK_NAME) => " (no such share)",
                // Samba's answer to an SMB 2.1 logon when it requires
                // encryption (`server smb encrypt = required`).
                (command::SESSION_SETUP, status::ACCESS_DENIED) => {
                    " (access denied; the server may require encryption, which needs SMB3)"
                }
                (_, status::ACCESS_DENIED) => " (access denied)",
                _ => "",
            };
            match status::name(*s) {
                Some(name) => format!("{}: {name}{what}", command::name(*c)),
                None => format!("{}: status {s:#010x}", command::name(*c)),
            }
        }
        Error::Transport(text) => text.clone(),
        Error::Closed => String::from("the server closed the connection"),
        Error::Malformed(what) => format!("malformed message from the server ({what})"),
        Error::Dialect(d) => format!("no common dialect (the server chose {d:#06x})"),
        Error::Signature => String::from("a response failed its signature check"),
        Error::Refused(what) => String::from(*what),
        Error::BadName => String::from("a name the client will not send"),
        Error::NoCredits => String::from("the server granted no credits"),
    }
}

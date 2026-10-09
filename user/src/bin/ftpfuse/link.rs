//! The FTP connection behind `ftpfuse`, and the inode numbers it hands out.
//!
//! A [`Link`] logs in lazily and again after the connection drops (servers
//! close idle control connections, and the guest's network may blink): a
//! command that fails for want of a connection is retried once on a fresh
//! one, and opening that connection is tried again for up to
//! [`RECONNECT_TICKS`], well inside the kernel's 10 s request deadline. A
//! server's refusal is not a connection failure and is never retried; it
//! becomes an errno.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use ftpwire::Reply;
use fused::daemon::Errno;
use fused::wire::errno;
use user::messenger::netsock::{Addr, Client};
use user::sys;

use crate::session::{drain, send_all, Control};

/// The largest file read or rewritten whole, bytes.
pub const MAX_FILE: usize = 32 * 1024 * 1024;
/// The largest listing read, bytes.
const MAX_LISTING: usize = 8 * 1024 * 1024;
/// How long one command keeps trying to open a connection, ticks (100 Hz).
const RECONNECT_TICKS: u64 = 500;
/// The pause between two connection attempts, nanoseconds.
const RECONNECT_PAUSE_NS: u64 = 250_000_000;

/// A command's outcome: the reply, or the server's refusal code. The outer
/// `Err` is a connection failure.
pub type Outcome = Result<Result<Reply, u16>, String>;

pub struct Link {
    sockets: Rc<Client>,
    peer: Addr,
    user: String,
    pass: String,
    verbose: bool,
    control: Option<Control>,
}

impl Link {
    pub fn new(sockets: Rc<Client>, peer: Addr, user: String, pass: String, verbose: bool) -> Link {
        Link {
            sockets,
            peer,
            user,
            pass,
            verbose,
            control: None,
        }
    }

    /// Open and log in now (so a wrong host or password fails the mount).
    pub fn connect(&mut self) -> Result<(), String> {
        let mut c = Control::open(&self.sockets, self.peer, self.verbose)?;
        let greeting = c.reply()?;
        if greeting.class() != 2 {
            return Err(alloc::format!(
                "greeting: {} {}",
                greeting.code,
                greeting.text()
            ));
        }
        if c.request("USER", Some(&self.user), &[2, 3])?.class() == 3 {
            c.request("PASS", Some(&self.pass), &[2])?;
        }
        c.request("TYPE", Some("I"), &[2])?;
        self.control = Some(c);
        Ok(())
    }

    pub fn connected(&self) -> bool {
        self.control.is_some()
    }

    /// Drop the connection; the next command opens a new one.
    pub fn drop_connection(&mut self) {
        self.control = None;
    }

    /// Run `step` on the control connection, once more on a fresh one if
    /// the connection failed (at most twice, so a non-idempotent step is not
    /// repeated further). Opening a connection is retried until
    /// [`RECONNECT_TICKS`] pass. A connection that never holds is `EIO`.
    pub fn run<T>(
        &mut self,
        step: impl FnMut(&mut Control) -> Result<T, String>,
    ) -> Result<T, Errno> {
        self.run_guarded(step, || true)
    }

    /// [`Link::run`], retrying a failed step only while `may_retry` says so
    /// (an append whose data phase began must not be sent twice).
    fn run_guarded<T>(
        &mut self,
        mut step: impl FnMut(&mut Control) -> Result<T, String>,
        may_retry: impl Fn() -> bool,
    ) -> Result<T, Errno> {
        let deadline = sys::clock() + RECONNECT_TICKS;
        let mut steps = 0;
        while steps < 2 {
            if self.control.is_none() && self.connect().is_err() {
                if sys::clock() >= deadline {
                    break;
                }
                sys::sleep_ns(RECONNECT_PAUSE_NS);
                continue;
            }
            steps += 1;
            let control = self.control.as_mut().expect("connected");
            match step(control) {
                Ok(value) => return Ok(value),
                Err(_) => self.control = None,
            }
            if !may_retry() {
                break;
            }
        }
        Err(errno::EIO)
    }

    /// One command: its reply when its class is in `classes`, the errno of
    /// the refusal otherwise.
    pub fn command(
        &mut self,
        verb: &str,
        argument: Option<&str>,
        classes: &[u16],
    ) -> Result<Reply, Errno> {
        let outcome = self.run(|c| ask(c, verb, argument, classes))?;
        outcome.map_err(errno_of)
    }

    /// `RNFR` then `RNTO`, on one connection.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), RenameError> {
        let outcome = self
            .run(|c| match ask(c, "RNFR", Some(from), &[3])? {
                Ok(_) => Ok(ask(c, "RNTO", Some(to), &[2])?.map_err(RenameError::Target)),
                Err(code) => Ok(Err(RenameError::Source(code))),
            })
            .map_err(RenameError::Link)?;
        outcome.map(|_| ())
    }

    /// A download (`RETR`, `MLSD`, `LIST`) read whole, at most `limit` bytes.
    pub fn fetch(&mut self, verb: &str, argument: &str, limit: usize) -> Result<Vec<u8>, Errno> {
        let outcome = self.run(|c| {
            let data = match open(c, verb, argument)? {
                Ok(data) => data,
                Err(code) => return Ok(Err(code)),
            };
            let mut body = Vec::new();
            let read = drain(&data, limit, |chunk| body.extend_from_slice(chunk));
            drop(data);
            read?;
            Ok(finish(c)?.map(|_| body))
        })?;
        outcome.map_err(errno_of)
    }

    /// A listing body of `remote`: `MLSD` when the server has it (decided
    /// once, by its first answer), `LIST` otherwise. `true` with `MLSD`.
    pub fn listing(
        &mut self,
        remote: &str,
        mlsd: &mut Option<bool>,
    ) -> Result<(Vec<u8>, bool), Errno> {
        if *mlsd != Some(false) {
            match self.fetch("MLSD", remote, MAX_LISTING) {
                Ok(body) => {
                    *mlsd = Some(true);
                    return Ok((body, true));
                }
                Err(errno::EOPNOTSUPP) if mlsd.is_none() => *mlsd = Some(false),
                Err(error) => return Err(error),
            }
        }
        self.fetch("LIST", remote, MAX_LISTING)
            .map(|body| (body, false))
    }

    /// An upload (`STOR`, `APPE`) of `bytes`.
    ///
    /// A `STOR` replaces the file whole, so it is retried like any command.
    /// An `APPE` is not idempotent: once its data connection is open, the
    /// server may have appended any part of `bytes` before the connection was
    /// lost, so a lost connection then is `EIO`, never a second append.
    pub fn store(&mut self, verb: &str, remote: &str, bytes: &[u8]) -> Result<(), Errno> {
        let started = Cell::new(false);
        let idempotent = verb != "APPE";
        let outcome = self.run_guarded(
            |c| {
                let data = match open(c, verb, remote)? {
                    Ok(data) => data,
                    Err(code) => return Ok(Err(code)),
                };
                started.set(true);
                let sent = send_all(&data, bytes);
                // End of file for the server is the end of the data connection.
                let _ = data.shutdown_write();
                drop(data);
                sent?;
                Ok(finish(c)?.map(|_| ()))
            },
            || idempotent || !started.get(),
        )?;
        outcome.map_err(errno_of)
    }
}

/// Why a rename failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameError {
    /// The connection failed (an errno).
    Link(Errno),
    /// The server refused `RNFR` (its code).
    Source(u16),
    /// The server refused `RNTO` (its code): the destination may exist.
    Target(u16),
}

impl RenameError {
    pub fn errno(self) -> Errno {
        match self {
            RenameError::Link(error) => error,
            RenameError::Source(code) | RenameError::Target(code) => errno_of(code),
        }
    }
}

/// Send a command and sort its reply.
fn ask(c: &mut Control, verb: &str, argument: Option<&str>, classes: &[u16]) -> Outcome {
    c.send(verb, argument)?;
    let reply = c.reply()?;
    Ok(if classes.contains(&reply.class()) {
        Ok(reply)
    } else {
        Err(reply.code)
    })
}

/// The reply that ends a transfer (226 or 250), or the server's refusal
/// (`552` over quota, `451` aborted, ...). A refusal here is the server's
/// answer, not a lost connection: it must not be retried, or an `APPE` whose
/// data the server kept would be appended twice.
fn finish(c: &mut Control) -> Outcome {
    let reply = c.reply()?;
    Ok(if reply.class() == 2 {
        Ok(reply)
    } else {
        Err(reply.code)
    })
}

/// `PASV` and a transfer command; the data connection, or the refusal.
fn open(
    c: &mut Control,
    verb: &str,
    argument: &str,
) -> Result<Result<user::messenger::netstd::TcpStream, u16>, String> {
    let data = c.passive()?;
    Ok(ask(c, verb, Some(argument), &[1])?.map(|_| data))
}

/// The errno of a server's refusal code.
pub fn errno_of(code: u16) -> Errno {
    match code {
        530 | 532 | 550 => errno::EACCES,
        452 | 552 => errno::ENOSPC,
        553 => errno::EINVAL,
        500..=502 | 504 => errno::EOPNOTSUPP,
        _ => errno::EIO,
    }
}

/// The remote path of a mount-relative path.
pub fn remote(path: &str) -> String {
    alloc::format!("/{path}")
}

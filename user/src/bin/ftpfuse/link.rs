//! The FTP connection behind `ftpfuse`, and the inode numbers it hands out.
//!
//! A [`Link`] logs in lazily and again after the connection drops (servers
//! close idle control connections): a command that fails for want of a
//! connection is retried once on a fresh one. A server's refusal is not a
//! connection failure and is never retried; it becomes an errno.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use ftpwire::Reply;
use fused::daemon::Errno;
use fused::wire::errno;
use user::messenger::netsock::{Addr, Client};

use crate::session::{drain, send_all, Control};

/// The largest file read or rewritten whole, bytes.
pub const MAX_FILE: usize = 32 * 1024 * 1024;
/// The largest listing read, bytes.
const MAX_LISTING: usize = 8 * 1024 * 1024;

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
    /// the connection failed. A connection failure both times is `EIO`.
    pub fn run<T>(
        &mut self,
        mut step: impl FnMut(&mut Control) -> Result<T, String>,
    ) -> Result<T, Errno> {
        for _ in 0..2 {
            if self.control.is_none() && self.connect().is_err() {
                continue;
            }
            let control = self.control.as_mut().expect("connected");
            match step(control) {
                Ok(value) => return Ok(value),
                Err(_) => self.control = None,
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
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        let outcome = self.run(|c| match ask(c, "RNFR", Some(from), &[3])? {
            Ok(_) => ask(c, "RNTO", Some(to), &[2]),
            refused => Ok(refused),
        })?;
        outcome.map(|_| ()).map_err(errno_of)
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
            c.finish_transfer()?;
            Ok(Ok(body))
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
    pub fn store(&mut self, verb: &str, remote: &str, bytes: &[u8]) -> Result<(), Errno> {
        let outcome = self.run(|c| {
            let data = match open(c, verb, remote)? {
                Ok(data) => data,
                Err(code) => return Ok(Err(code)),
            };
            let sent = send_all(&data, bytes);
            // End of file for the server is the end of the data connection.
            let _ = data.shutdown_write();
            drop(data);
            sent?;
            c.finish_transfer()?;
            Ok(Ok(()))
        })?;
        outcome.map_err(errno_of)
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

/// Inode numbers by path: stable while the daemon runs, moved by renames,
/// never reused (so a node handle of a deleted file stays stale).
pub struct Inodes {
    by_path: BTreeMap<String, u64>,
    by_ino: BTreeMap<u64, String>,
    next: u64,
}

impl Inodes {
    pub const ROOT: u64 = 1;

    pub fn new() -> Inodes {
        let mut inodes = Inodes {
            by_path: BTreeMap::new(),
            by_ino: BTreeMap::new(),
            next: Self::ROOT + 1,
        };
        inodes.by_path.insert(String::new(), Self::ROOT);
        inodes.by_ino.insert(Self::ROOT, String::new());
        inodes
    }

    pub fn ino(&mut self, path: &str) -> u64 {
        if let Some(&ino) = self.by_path.get(path) {
            return ino;
        }
        let ino = self.next;
        self.next += 1;
        self.by_path.insert(String::from(path), ino);
        self.by_ino.insert(ino, String::from(path));
        ino
    }

    pub fn path(&self, ino: u64) -> Option<&str> {
        self.by_ino.get(&ino).map(String::as_str)
    }

    /// Forget `path` and everything below it.
    pub fn forget(&mut self, path: &str) {
        for (old, ino) in self.below(path) {
            self.by_path.remove(&old);
            self.by_ino.remove(&ino);
        }
    }

    /// `from` (and everything below it) is now `to`.
    pub fn rename(&mut self, from: &str, to: &str) {
        self.forget(to);
        for (old, ino) in self.below(from) {
            let new = alloc::format!("{to}{}", &old[from.len()..]);
            self.by_path.remove(&old);
            self.by_path.insert(new.clone(), ino);
            self.by_ino.insert(ino, new);
        }
    }

    fn below(&self, path: &str) -> Vec<(String, u64)> {
        self.by_path
            .iter()
            .filter(|(p, _)| {
                p.as_str() == path
                    || (p.starts_with(path) && p.as_bytes().get(path.len()) == Some(&b'/'))
            })
            .map(|(p, &ino)| (p.clone(), ino))
            .collect()
    }
}

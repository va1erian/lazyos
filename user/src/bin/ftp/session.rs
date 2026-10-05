//! The control connection and the passive data connections of `ftp` and
//! `ftpfuse` (which includes this file too).
//!
//! Every byte a server sends goes through `ftpwire` before it is believed, and
//! every command is built by `ftpwire::command`, which refuses an argument that
//! could carry a second command. The data connection always goes to the
//! *control* peer's address (only the port comes from the `227` reply), so a
//! server cannot send the client to a third host.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

use ftpwire::{command, parse_pasv, Reply, ReplyParser};
use user::messenger::netsock::{Addr, Client};
use user::messenger::netstd::{is_timeout, TcpStream};
use user::messenger::Error as MsgError;
use user::sys;

/// Milliseconds to wait for the connection to open.
const CONNECT_MS: u32 = 8000;
/// Milliseconds a reply may take.
const REPLY_MS: u64 = 20_000;
/// Milliseconds one read waits before the clock is checked.
const SLICE_MS: u32 = 200;
/// Milliseconds a data transfer may stall.
const STALL_MS: u64 = 15_000;

pub(super) fn describe(error: &MsgError) -> String {
    use user::messenger::netsock::errno as e;
    match error {
        MsgError::Errno(code) if *code == -e::ECONNREFUSED => String::from("connection refused"),
        MsgError::Errno(code) if *code == -e::ENETUNREACH => String::from("network is unreachable"),
        MsgError::Errno(code) if *code == -e::ECONNRESET => String::from("connection reset"),
        MsgError::Errno(code) if *code == -user::messenger::errno::ETIMEDOUT => {
            String::from("timed out")
        }
        other => String::from(other.message()),
    }
}

pub(super) struct Control {
    sockets: Rc<Client>,
    stream: TcpStream,
    parser: ReplyParser,
    peer: Addr,
    /// Echo every command and reply to the console.
    pub(super) verbose: bool,
}

impl Control {
    pub(super) fn open(sockets: &Rc<Client>, peer: Addr, verbose: bool) -> Result<Control, String> {
        let stream = TcpStream::connect(sockets, peer, CONNECT_MS)
            .map_err(|e| format!("connect: {}", describe(&e)))?;
        Ok(Control {
            sockets: sockets.clone(),
            stream,
            parser: ReplyParser::new(),
            peer,
            verbose,
        })
    }

    /// The next complete reply, waiting up to [`REPLY_MS`].
    pub(super) fn reply(&mut self) -> Result<Reply, String> {
        let deadline = sys::clock() + REPLY_MS / 10;
        loop {
            if let Some(reply) = self.parser.next() {
                if self.verbose {
                    for line in &reply.lines {
                        sys::write_str(&format!("< {} {line}\n", reply.code));
                    }
                }
                return Ok(reply);
            }
            if sys::clock() >= deadline {
                return Err(String::from("no reply from the server"));
            }
            match self.stream.read(4096, SLICE_MS) {
                Ok(chunk) if chunk.is_empty() => {
                    return Err(String::from("the server closed the connection"))
                }
                Ok(chunk) => self
                    .parser
                    .feed(&chunk)
                    .map_err(|e| format!("malformed reply from the server ({e:?})"))?,
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(format!("control read: {}", describe(&e))),
            }
        }
    }

    pub(super) fn send(&mut self, verb: &str, argument: Option<&str>) -> Result<(), String> {
        let line = command(verb, argument).map_err(|e| format!("{verb}: {e:?}"))?;
        if self.verbose {
            // Never echo a password.
            let shown = if verb == "PASS" {
                "****"
            } else {
                argument.unwrap_or("")
            };
            sys::write_str(&format!("> {verb} {shown}\n"));
        }
        self.stream
            .write_all(&line, 5000)
            .map_err(|e| format!("control write: {}", describe(&e)))
    }

    /// Send a command and require a reply of one of `classes` (first digit).
    pub(super) fn request(
        &mut self,
        verb: &str,
        argument: Option<&str>,
        classes: &[u16],
    ) -> Result<Reply, String> {
        self.send(verb, argument)?;
        let reply = self.reply()?;
        if classes.contains(&reply.class()) {
            Ok(reply)
        } else {
            Err(format!("{verb}: {} {}", reply.code, reply.text()))
        }
    }

    /// `PASV` and the connection it describes.
    pub(super) fn passive(&mut self) -> Result<TcpStream, String> {
        let reply = self.request("PASV", None, &[2])?;
        let (_, port) = parse_pasv(reply.text())
            .ok_or_else(|| format!("PASV: unusable reply {:?}", reply.text()))?;
        // The address in the reply is ignored on purpose: see the module docs.
        let to = Addr::new(self.peer.ip, port);
        TcpStream::connect(&self.sockets, to, CONNECT_MS)
            .map_err(|e| format!("data connection to port {port}: {}", describe(&e)))
    }

    /// The reply that ends a transfer (226 or 250).
    pub(super) fn finish_transfer(&mut self) -> Result<(), String> {
        let reply = self.reply()?;
        if reply.class() == 2 {
            Ok(())
        } else {
            Err(format!("transfer: {} {}", reply.code, reply.text()))
        }
    }

    /// Start a transfer command on a fresh passive connection; the
    /// preliminary reply (125 or 150) has been read.
    pub(super) fn transfer(
        &mut self,
        verb: &str,
        argument: Option<&str>,
    ) -> Result<TcpStream, String> {
        let data = self.passive()?;
        self.request(verb, argument, &[1])?;
        Ok(data)
    }

    pub(super) fn quit(&mut self) {
        let _ = self.send("QUIT", None);
        let _ = self.reply();
    }
}

/// Read a data connection to its end, handing each chunk to `sink`. A stall of
/// [`STALL_MS`] is an error; so is more than `limit` bytes.
pub(super) fn drain(
    data: &TcpStream,
    limit: usize,
    mut sink: impl FnMut(&[u8]),
) -> Result<usize, String> {
    let mut total = 0usize;
    let mut idle_until = sys::clock() + STALL_MS / 10;
    loop {
        match data.read(16 * 1024, SLICE_MS) {
            Ok(chunk) if chunk.is_empty() => return Ok(total),
            Ok(chunk) => {
                total += chunk.len();
                if total > limit {
                    return Err(format!("more than {limit} bytes"));
                }
                sink(&chunk);
                idle_until = sys::clock() + STALL_MS / 10;
            }
            Err(e) if is_timeout(&e) => {
                if sys::clock() >= idle_until {
                    return Err(String::from("the transfer stalled"));
                }
            }
            Err(e) => return Err(format!("data read: {}", describe(&e))),
        }
    }
}

/// Write all of `bytes` to a data connection; a stall of [`STALL_MS`] is an
/// error.
pub(super) fn send_all(data: &TcpStream, bytes: &[u8]) -> Result<(), String> {
    let mut at = 0;
    let mut stalled = sys::clock() + STALL_MS / 10;
    while at < bytes.len() {
        match data.write_some(&bytes[at..], SLICE_MS) {
            Ok(k) => {
                at += k;
                stalled = sys::clock() + STALL_MS / 10;
            }
            Err(e) if is_timeout(&e) && sys::clock() < stalled => {}
            Err(e) => return Err(format!("data write: {}", describe(&e))),
        }
    }
    Ok(())
}

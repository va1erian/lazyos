//! `unixstress` — `std::os::unix::net`: `UnixStream::pair` read/write, EOF on
//! drop, `shutdown` half-close, pathname `UnixListener` bind/connect/accept,
//! and a raw `SOCK_SEQPACKET` message-boundary check.

mod common;

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};

unsafe extern "C" {
    fn socketpair(domain: i32, kind: i32, protocol: i32, sv: *mut i32) -> i32;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn close(fd: i32) -> i32;
}

const AF_UNIX: i32 = 1;
const SOCK_SEQPACKET: i32 = 5;
const EMSGSIZE: i32 = 90;

const PATH: &str = "/tmp/abi-unixstress.sock";

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn errno() -> Option<i32> {
    std::io::Error::last_os_error().raw_os_error()
}

fn main() {
    let mut first: Option<String> = None;

    // UnixStream::pair: bidirectional bytes.
    match UnixStream::pair() {
        Ok((mut a, mut b)) => {
            if a.write_all(b"ping").is_err() {
                note(&mut first, "pair write failed".to_string());
            }
            let mut buf = [0u8; 8];
            if b.read_exact(&mut buf[..4]).is_err() || &buf[..4] != b"ping" {
                note(&mut first, "pair read failed".to_string());
            }
        }
        Err(err) => note(&mut first, format!("UnixStream::pair: {err}")),
    }

    // EOF on drop: the peer reads what was buffered, then zero.
    match UnixStream::pair() {
        Ok((mut a, mut b)) => {
            if a.write_all(b"bye").is_err() {
                note(&mut first, "drop-EOF write failed".to_string());
            }
            drop(a);
            let mut buf = [0u8; 8];
            match b.read(&mut buf) {
                Ok(3) if &buf[..3] == b"bye" => {}
                Ok(n) => note(&mut first, format!("drop-EOF first read returned {n}")),
                Err(err) => note(&mut first, format!("drop-EOF first read: {err}")),
            }
            match b.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => note(&mut first, format!("drop-EOF read returned {n}, expected 0")),
                Err(err) => note(&mut first, format!("drop-EOF read: {err}")),
            }
        }
        Err(err) => note(&mut first, format!("drop-EOF pair: {err}")),
    }

    // shutdown(SHUT_WR): the peer sees EOF, this end still reads.
    match UnixStream::pair() {
        Ok((mut a, mut b)) => {
            if a.shutdown(Shutdown::Write).is_err() {
                note(&mut first, "shutdown(Write) failed".to_string());
            }
            let mut buf = [0u8; 8];
            match b.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => note(&mut first, format!("half-close read returned {n}")),
                Err(err) => note(&mut first, format!("half-close read: {err}")),
            }
            if b.write_all(b"pong").is_err() {
                note(&mut first, "peer write after half-close failed".to_string());
            }
            if a.read_exact(&mut buf[..4]).is_err() || &buf[..4] != b"pong" {
                note(&mut first, "half-closed end could not read".to_string());
            }
        }
        Err(err) => note(&mut first, format!("shutdown pair: {err}")),
    }

    // Pathname listener: bind, connect, accept, exchange.
    match UnixListener::bind(PATH) {
        Ok(listener) => {
            if listener.local_addr().is_err() {
                note(&mut first, "local_addr failed".to_string());
            }
            match UnixStream::connect(PATH) {
                Ok(mut client) => match listener.accept() {
                    Ok((mut server, _addr)) => {
                        if client.write_all(b"hello").is_err() {
                            note(&mut first, "pathname client write failed".to_string());
                        }
                        let mut buf = [0u8; 8];
                        if server.read_exact(&mut buf[..5]).is_err() || &buf[..5] != b"hello" {
                            note(&mut first, "pathname server read failed".to_string());
                        }
                        if server.write_all(b"world").is_err() {
                            note(&mut first, "pathname server write failed".to_string());
                        }
                        if client.read_exact(&mut buf[..5]).is_err() || &buf[..5] != b"world" {
                            note(&mut first, "pathname client read failed".to_string());
                        }
                    }
                    Err(err) => note(&mut first, format!("accept: {err}")),
                },
                Err(err) => note(&mut first, format!("connect: {err}")),
            }
        }
        Err(err) => note(&mut first, format!("bind: {err}")),
    }

    // Connecting to a name nobody bound fails cleanly.
    if UnixStream::connect("/tmp/abi-unixstress-absent.sock").is_ok() {
        note(&mut first, "connect to an unbound name succeeded".to_string());
    }

    // Raw SOCK_SEQPACKET: one read per message, truncation discards the rest.
    let mut sv = [0i32; 2];
    if unsafe { socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv.as_mut_ptr()) } != 0 {
        note(&mut first, "seqpacket socketpair failed".to_string());
    } else {
        let (a, b) = (sv[0], sv[1]);
        if unsafe { send(a, b"hello".as_ptr(), 5, 0) } != 5 {
            note(&mut first, "seqpacket send failed".to_string());
        }
        let mut small = [0u8; 3];
        let n = unsafe { recv(b, small.as_mut_ptr(), small.len(), 0) };
        if n != 3 || &small != b"hel" {
            note(&mut first, format!("seqpacket truncation returned {n}"));
        }
        // The discarded tail must not leak into the next message.
        if unsafe { send(a, b"xy".as_ptr(), 2, 0) } != 2 {
            note(&mut first, "seqpacket second send failed".to_string());
        }
        let mut big = [0u8; 8];
        let n = unsafe { recv(b, big.as_mut_ptr(), big.len(), 0) };
        if n != 2 || &big[..2] != b"xy" {
            note(&mut first, "seqpacket boundary lost after truncation".to_string());
        }
        // Back-to-back messages stay distinct.
        let _ = unsafe { send(a, b"one".as_ptr(), 3, 0) };
        let _ = unsafe { send(a, b"two".as_ptr(), 3, 0) };
        let n = unsafe { recv(b, big.as_mut_ptr(), big.len(), 0) };
        if n != 3 || &big[..3] != b"one" {
            note(&mut first, "seqpacket first message wrong".to_string());
        }
        let n = unsafe { recv(b, big.as_mut_ptr(), big.len(), 0) };
        if n != 3 || &big[..3] != b"two" {
            note(&mut first, "seqpacket second message wrong".to_string());
        }
        // Over-capacity messages are refused whole with EMSGSIZE.
        let huge = vec![0u8; 64 * 1024 + 1];
        let n = unsafe { send(a, huge.as_ptr(), huge.len(), 0) };
        if n != -1 || errno() != Some(EMSGSIZE) {
            note(&mut first, format!("oversized seqpacket send -> {n}"));
        }
        unsafe {
            close(a);
            close(b);
        }
    }

    match first {
        Some(reason) => common::fail("unixstress", &reason),
        None => common::pass("unixstress"),
    }
}

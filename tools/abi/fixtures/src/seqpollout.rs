//! `seqpollout` — `POLLOUT` on a `SOCK_SEQPACKET` socket (issue #863).
//!
//! WebKit's IPC sender (`ConnectionUnix::sendOutputMessage`) loops
//! `sendmsg` -> `EAGAIN` -> `poll(POLLOUT, -1)` -> `sendmsg`. When `poll`
//! says "writable" while the all-or-nothing send still cannot fit, the loop
//! spins and starves the slow reader it waits for. Here a non-blocking sender
//! thread runs that loop against a reader that takes one message per
//! millisecond; every `sendmsg` that follows a `POLLOUT` must succeed (no
//! spurious wake), and the sender may not wake more often than it sends.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn socketpair(domain: i32, kind: i32, protocol: i32, sv: *mut i32) -> i32;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
    fn close(fd: i32) -> i32;
}

const AF_UNIX: i32 = 1;
const SOCK_SEQPACKET: i32 = 5;
const SOCK_NONBLOCK: i32 = 0o4000;
const POLLOUT: i16 = 4;
const MSG_NOSIGNAL: i32 = 0x4000;
const EAGAIN: i32 = 11;

const FRAMES: u32 = 300;
const FRAME_LEN: usize = 4000;

fn errno() -> Option<i32> {
    std::io::Error::last_os_error().raw_os_error()
}

fn main() {
    let mut sv = [0i32; 2];
    // SOCK_NONBLOCK applies to both ends; the reader below copes with EAGAIN.
    if unsafe { socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK, 0, sv.as_mut_ptr()) } != 0 {
        common::fail("seqpollout", "socketpair failed");
    }
    let (tx, rx) = (sv[0], sv[1]);
    let done = Arc::new(AtomicBool::new(false));

    let reader = {
        let done = done.clone();
        thread::spawn(move || -> Result<u32, String> {
            let mut buf = vec![0u8; FRAME_LEN + 16];
            let mut expect = 0u32;
            while expect < FRAMES {
                let n = unsafe { recv(rx, buf.as_mut_ptr(), buf.len(), 0) };
                if n == -1 && errno() == Some(EAGAIN) {
                    // Nothing queued yet: wait a moment and retry.
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                if n != FRAME_LEN as isize {
                    return Err(format!("recv returned {n} for frame {expect}"));
                }
                let seq = u32::from_le_bytes(buf[..4].try_into().unwrap());
                if seq != expect {
                    return Err(format!("frame {seq} arrived, wanted {expect}"));
                }
                expect += 1;
                thread::sleep(Duration::from_millis(1));
            }
            done.store(true, Ordering::Release);
            Ok(expect)
        })
    };

    let mut frame = vec![0xa5u8; FRAME_LEN];
    let (mut waits, mut spurious) = (0u32, 0u32);
    for seq in 0..FRAMES {
        frame[..4].copy_from_slice(&seq.to_le_bytes());
        let mut after_pollout = false;
        loop {
            let n = unsafe { send(tx, frame.as_ptr(), frame.len(), MSG_NOSIGNAL) };
            if n == FRAME_LEN as isize {
                break;
            }
            if n != -1 || errno() != Some(EAGAIN) {
                common::fail("seqpollout", &format!("send returned {n}: {:?}", errno()));
            }
            if after_pollout {
                spurious += 1;
            }
            let mut entry = PollFd {
                fd: tx,
                events: POLLOUT,
                revents: 0,
            };
            if unsafe { poll(&mut entry, 1, 10_000) } != 1 || entry.revents & POLLOUT == 0 {
                common::fail("seqpollout", "poll(POLLOUT) timed out or failed");
            }
            waits += 1;
            after_pollout = true;
        }
    }

    match reader.join() {
        Ok(Ok(_)) => {}
        Ok(Err(reason)) => common::fail("seqpollout", &reason),
        Err(_) => common::fail("seqpollout", "reader thread panicked"),
    }
    unsafe {
        close(tx);
        close(rx);
    }
    if !done.load(Ordering::Acquire) {
        common::fail("seqpollout", "the reader did not finish");
    }
    if waits == 0 {
        common::fail(
            "seqpollout",
            "the sender never had to wait: the test is vacuous",
        );
    }
    if spurious != 0 {
        common::fail(
            "seqpollout",
            &format!("{spurious} sendmsg calls failed with EAGAIN right after POLLOUT"),
        );
    }
    if waits > FRAMES {
        common::fail(
            "seqpollout",
            &format!("{waits} poll wakeups for {FRAMES} frames"),
        );
    }
    common::pass("seqpollout");
}

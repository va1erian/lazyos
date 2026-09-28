//! `epollstress` — `eventfd2`, `epoll_create1`/`epoll_ctl`/`epoll_wait`:
//! level trigger, `EPOLLET` edges, timeouts, many descriptors, and the
//! add/mod/del lifecycle.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Instant;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct EpollEvent {
    events: u32,
    data: u64,
}

unsafe extern "C" {
    fn eventfd(initval: u32, flags: i32) -> i32;
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
    fn epoll_wait(epfd: i32, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i32;
    fn pipe2(fds: *mut i32, flags: i32) -> i32;
}

const O_NONBLOCK: i32 = 0o4000;
const O_CLOEXEC: i32 = 0o2000000;

const EPOLLIN: u32 = 0x0001;
const EPOLLOUT: u32 = 0x0004;
const EPOLLET: u32 = 0x8000_0000;
const EPOLL_CTL_ADD: i32 = 1;
const EPOLL_CTL_DEL: i32 = 2;
const EPOLL_CTL_MOD: i32 = 3;

const EFD_NONBLOCK: i32 = O_NONBLOCK;
const EFD_CLOEXEC: i32 = O_CLOEXEC;

const EEXIST: i32 = 17;
const ENOENT: i32 = 2;

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

/// An eventfd wrapped so its descriptor closes on drop.
fn make_eventfd(init: u32) -> Option<File> {
    let raw = unsafe { eventfd(init, EFD_NONBLOCK | EFD_CLOEXEC) };
    if raw < 0 {
        return None;
    }
    // Safety: `raw` is a fresh descriptor this process owns.
    Some(unsafe { File::from(OwnedFd::from_raw_fd(raw)) })
}

/// A pipe wrapped so both descriptors close on drop.
fn make_pipe() -> Option<(File, File)> {
    let mut fds = [0i32; 2];
    if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } != 0 {
        return None;
    }
    // Safety: both are fresh descriptors this process owns.
    unsafe {
        Some((
            File::from(OwnedFd::from_raw_fd(fds[0])),
            File::from(OwnedFd::from_raw_fd(fds[1])),
        ))
    }
}

fn ctl(epfd: RawFd, op: i32, fd: RawFd, events: u32, data: u64) -> i32 {
    let mut event = EpollEvent { events, data };
    unsafe { epoll_ctl(epfd, op, fd, &mut event) }
}

fn wait(epfd: RawFd, out: &mut [EpollEvent], timeout: i32) -> i32 {
    unsafe { epoll_wait(epfd, out.as_mut_ptr(), out.len() as i32, timeout) }
}

fn main() {
    let mut first: Option<String> = None;

    // epoll_create1: bad flags are refused, and the valid call returns an fd.
    if unsafe { epoll_create1(0x1234) } != -1 {
        note(&mut first, "epoll_create1 accepted unknown flags".to_string());
    }
    let epfd = unsafe { epoll_create1(O_CLOEXEC) };
    if epfd < 0 {
        common::fail("epollstress", "epoll_create1 failed");
    }

    // eventfd: level-triggered readiness, drain, then MOD to EPOLLOUT.
    let mut efd = match make_eventfd(0) {
        Some(file) => file,
        None => common::fail("epollstress", "eventfd failed"),
    };
    let mut events = [EpollEvent { events: 0, data: 0 }; 4];
    if ctl(epfd, EPOLL_CTL_ADD, efd.as_raw_fd(), EPOLLIN, 0x11) != 0 {
        common::fail("epollstress", "ADD eventfd failed");
    }
    if ctl(epfd, EPOLL_CTL_ADD, efd.as_raw_fd(), EPOLLIN, 0x11) != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(EEXIST)
    {
        note(&mut first, "duplicate ADD was not EEXIST".to_string());
    }
    if wait(epfd, &mut events, 0) != 0 {
        note(&mut first, "idle epoll_wait reported events".to_string());
    }
    let one = 1u64.to_ne_bytes();
    if efd.write(&one).ok() != Some(8) {
        note(&mut first, "eventfd write failed".to_string());
    }
    if wait(epfd, &mut events, 100) != 1 {
        note(&mut first, "ready eventfd not reported".to_string());
    } else {
        let bits = events[0].events;
        let data = events[0].data;
        if bits & EPOLLIN == 0 || data != 0x11 {
            note(&mut first, format!("eventfd event flags={bits:#x} data={data:#x}"));
        }
        // Level trigger: still ready while the counter is non-zero.
        if wait(epfd, &mut events, 0) != 1 {
            note(&mut first, "level-triggered eventfd drained early".to_string());
        }
    }
    let mut value = [0u8; 8];
    if efd.read(&mut value).ok() != Some(8) || u64::from_ne_bytes(value) != 1 {
        note(&mut first, "eventfd read did not return the counter".to_string());
    }
    if wait(epfd, &mut events, 0) != 0 {
        note(&mut first, "drained eventfd still ready".to_string());
    }
    if ctl(epfd, EPOLL_CTL_MOD, efd.as_raw_fd(), EPOLLOUT, 0x12) != 0 {
        note(&mut first, "MOD eventfd failed".to_string());
    }
    if wait(epfd, &mut events, 0) != 1 || events[0].events & EPOLLOUT == 0 {
        note(&mut first, "MOD did not switch the interest to EPOLLOUT".to_string());
    }
    if ctl(epfd, EPOLL_CTL_DEL, efd.as_raw_fd(), EPOLLIN, 0) != 0 {
        note(&mut first, "DEL eventfd failed".to_string());
    }
    if ctl(epfd, EPOLL_CTL_DEL, efd.as_raw_fd(), EPOLLIN, 0) != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(ENOENT)
    {
        note(&mut first, "duplicate DEL was not ENOENT".to_string());
    }

    // A pipe: level trigger and a bounded timeout on an idle wait.
    let (mut rd, mut wr) = match make_pipe() {
        Some(pair) => pair,
        None => common::fail("epollstress", "pipe2 failed"),
    };
    if ctl(epfd, EPOLL_CTL_ADD, rd.as_raw_fd(), EPOLLIN, 0x21) != 0 {
        common::fail("epollstress", "ADD pipe failed");
    }
    let started = Instant::now();
    if wait(epfd, &mut events, 50) != 0 {
        note(&mut first, "idle pipe reported ready".to_string());
    }
    if started.elapsed().as_millis() < 40 {
        note(&mut first, "timeout fired early".to_string());
    }
    if wr.write(b"x").ok() != Some(1) {
        note(&mut first, "pipe write failed".to_string());
    }
    if wait(epfd, &mut events, 0) != 1 || events[0].events & EPOLLIN == 0 {
        note(&mut first, "readable pipe not reported".to_string());
    }
    if wait(epfd, &mut events, 0) != 1 {
        note(&mut first, "level trigger did not repeat".to_string());
    }
    let mut byte = [0u8; 1];
    let _ = rd.read(&mut byte);
    if wait(epfd, &mut events, 0) != 0 {
        note(&mut first, "drained pipe still ready".to_string());
    }

    // EPOLLET: one report per change, and fresh bytes re-arm the edge even
    // while the pipe stays readable.
    if ctl(epfd, EPOLL_CTL_MOD, rd.as_raw_fd(), EPOLLIN | EPOLLET, 0x22) != 0 {
        note(&mut first, "MOD to EPOLLET failed".to_string());
    }
    if wr.write(b"ab").ok() != Some(2) {
        note(&mut first, "edge write failed".to_string());
    }
    if wait(epfd, &mut events, 0) != 1 {
        note(&mut first, "edge did not report on arm".to_string());
    }
    if wait(epfd, &mut events, 0) != 0 {
        note(&mut first, "edge repeated without a change".to_string());
    }
    let _ = rd.read(&mut byte); // one byte of two: still readable
    if wr.write(b"c").ok() != Some(1) {
        note(&mut first, "edge second write failed".to_string());
    }
    if wait(epfd, &mut events, 0) != 1 {
        note(&mut first, "new data did not re-arm the edge".to_string());
    }
    let mut drain = [0u8; 4];
    let _ = rd.read(&mut drain);
    if ctl(epfd, EPOLL_CTL_MOD, rd.as_raw_fd(), EPOLLIN, 0x23) != 0 {
        note(&mut first, "MOD back to level failed".to_string());
    }

    // Many descriptors: several eventfds and a second pipe, partially ready.
    let mut many: Vec<File> = Vec::new();
    for n in 1..=4u32 {
        match make_eventfd(n) {
            Some(file) => many.push(file),
            None => note(&mut first, "many eventfds failed".to_string()),
        }
    }
    let (rd2, mut wr2) = match make_pipe() {
        Some(pair) => pair,
        None => common::fail("epollstress", "second pipe2 failed"),
    };
    for (index, file) in many.iter().enumerate() {
        if ctl(epfd, EPOLL_CTL_ADD, file.as_raw_fd(), EPOLLIN, index as u64) != 0 {
            note(&mut first, "ADD many eventfd failed".to_string());
        }
    }
    if ctl(epfd, EPOLL_CTL_ADD, rd2.as_raw_fd(), EPOLLIN, 0x30) != 0 {
        note(&mut first, "ADD second pipe failed".to_string());
    }
    if wr2.write(b"z").ok() != Some(1) {
        note(&mut first, "second pipe write failed".to_string());
    }
    let mut many_events = [EpollEvent { events: 0, data: 0 }; 8];
    let ready = wait(epfd, &mut many_events, 100);
    if ready != 5 {
        let seen: Vec<u64> = many_events[..ready.max(0) as usize]
            .iter()
            .map(|event| event.data)
            .collect();
        note(
            &mut first,
            format!("many-fd wait returned {ready}, expected 5: {seen:?}"),
        );
    }
    // A capped wait returns at most `maxevents`.
    let capped = wait(epfd, &mut many_events[..1], 0);
    if capped != 1 {
        note(&mut first, format!("capped wait returned {capped}, expected 1"));
    }

    // Cleanup: every interest is removable and every fd closes.
    for file in many.iter() {
        if ctl(epfd, EPOLL_CTL_DEL, file.as_raw_fd(), EPOLLIN, 0) != 0 {
            note(&mut first, "DEL many eventfd failed".to_string());
        }
    }
    if ctl(epfd, EPOLL_CTL_DEL, rd2.as_raw_fd(), EPOLLIN, 0) != 0 {
        note(&mut first, "DEL second pipe failed".to_string());
    }
    if ctl(epfd, EPOLL_CTL_DEL, rd.as_raw_fd(), EPOLLIN, 0) != 0 {
        note(&mut first, "DEL first pipe failed".to_string());
    }
    drop(many);
    drop(rd2);
    drop(wr2);
    drop(rd);
    drop(wr);
    drop(efd);

    // Safety: `epfd` is this process's epoll descriptor.
    unsafe { libc_close(epfd) };

    match first {
        Some(reason) => common::fail("epollstress", &reason),
        None => common::pass("epollstress"),
    }
}

unsafe extern "C" {
    #[link_name = "close"]
    fn libc_close(fd: i32) -> i32;
}

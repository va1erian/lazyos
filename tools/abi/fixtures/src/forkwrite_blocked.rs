//! `forkwrite`'s blocked threads: calls asleep in the kernel while another thread forks.
//!
//! Three threads wait in the kernel with their user buffer on one page, a pipe `read`, a
//! `SOCK_SEQPACKET` `recv` and a `poll` (its `pollfd`) followed by a `read`, the way WebKit's IPC and
//! run-loop threads wait. The launcher forks while they are asleep, a fourth thread keeps storing
//! to the same page (so the parent takes a copy-on-write fault on it after every fork), and only
//! then is each call given its data: the kernel must deliver it to the page the parent maps now,
//! not to the frame it mapped when the call began (by then the child's, or freed by its `execve`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn socketpair(domain: i32, kind: i32, protocol: i32, sv: *mut i32) -> i32;
    fn read(fd: i32, buf: *mut u8, len: usize) -> isize;
    fn write(fd: i32, buf: *const u8, len: usize) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
}

const AF_UNIX: i32 = 1;
const SOCK_SEQPACKET: i32 = 5;
const POLLIN: i16 = 1;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

/// The one page every waiting call writes into, and the word the toucher stores to.
#[repr(C, align(4096))]
struct Page {
    touch: u64,
    pipe_buf: [u8; 8],
    sock_buf: [u8; 8],
    poll_fd: PollFd,
    poll_buf: [u8; 8],
}

/// The page's address: the threads share it on purpose (each writes its own fields).
#[derive(Clone, Copy)]
struct PagePtr(*mut Page);
// SAFETY: every field has one writer (its thread, or the kernel on that thread's behalf); the
// launcher reads only the round counters, which are atomics.
unsafe impl Send for PagePtr {}

pub struct Blocked {
    pipe_w: i32,
    sock_w: i32,
    poll_w: i32,
    /// The last round each reader (pipe, socket, poll) saw correctly.
    done: Arc<[AtomicU64; 3]>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

fn reader(
    which: usize,
    page: PagePtr,
    fd: i32,
    done: Arc<[AtomicU64; 3]>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
) {
    let names = ["pipe read", "seqpacket recv", "poll"];
    for round in 1u64.. {
        let p = page.0;
        // SAFETY: `p` is the page `start` leaked; this thread is the only user of its fields.
        let got = unsafe {
            let buf = match which {
                0 => (*p).pipe_buf.as_mut_ptr(),
                1 => (*p).sock_buf.as_mut_ptr(),
                _ => (*p).poll_buf.as_mut_ptr(),
            };
            std::ptr::write_bytes(buf, 0, 8);
            if which == 2 {
                (*p).poll_fd = PollFd {
                    fd,
                    events: POLLIN,
                    revents: 0,
                };
                if poll(&mut (*p).poll_fd, 1, -1) != 1 || (*p).poll_fd.revents & POLLIN == 0 {
                    let revents = (*p).poll_fd.revents;
                    *failure.lock().unwrap() = Some(format!(
                        "round {round}: poll's revents {revents:#x} were lost"
                    ));
                    return;
                }
            }
            let n = if which == 1 {
                recv(fd, buf, 8, 0)
            } else {
                read(fd, buf, 8)
            };
            if n == 0 {
                return; // the writer closed: the run is over
            }
            if n != 8 {
                *failure.lock().unwrap() =
                    Some(format!("round {round}: {} returned {n}", names[which]));
                return;
            }
            u64::from_ne_bytes(std::ptr::read_volatile(buf as *const [u8; 8]))
        };
        if got != round {
            *failure.lock().unwrap() = Some(format!(
                "round {round}: {} delivered {got:#x} to the page",
                names[which]
            ));
            return;
        }
        done[which].store(round, Ordering::Release);
    }
}

fn spawn_reader(which: usize, page: PagePtr, fd: i32, b: &Blocked) {
    let (done, failure) = (b.done.clone(), b.failure.clone());
    std::thread::spawn(move || reader(which, page, fd, done, failure));
}

/// The readers and the toucher, started; None when a descriptor could not be made.
pub fn start() -> Option<Blocked> {
    let (mut p1, mut p2, mut s) = ([0i32; 2], [0i32; 2], [0i32; 2]);
    // SAFETY: each call fills the two-element array it is given.
    if unsafe {
        pipe(p1.as_mut_ptr()) != 0
            || pipe(p2.as_mut_ptr()) != 0
            || socketpair(AF_UNIX, SOCK_SEQPACKET, 0, s.as_mut_ptr()) != 0
    } {
        return None;
    }
    let page = PagePtr(Box::leak(Box::new(Page {
        touch: 0,
        pipe_buf: [0; 8],
        sock_buf: [0; 8],
        poll_fd: PollFd {
            fd: -1,
            events: 0,
            revents: 0,
        },
        poll_buf: [0; 8],
    })));
    let blocked = Blocked {
        pipe_w: p1[1],
        sock_w: s[0],
        poll_w: p2[1],
        done: Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]),
        failure: Arc::new(std::sync::Mutex::new(None)),
        stop: Arc::new(AtomicBool::new(false)),
    };
    spawn_reader(0, page, p1[0], &blocked);
    spawn_reader(1, page, s[1], &blocked);
    spawn_reader(2, page, p2[0], &blocked);
    let stop = blocked.stop.clone();
    std::thread::spawn(move || {
        let page = page;
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: the toucher is the only writer of `touch`; volatile, so every store happens.
            unsafe {
                std::ptr::write_volatile(&mut (*page.0).touch, (*page.0).touch.wrapping_add(1))
            };
        }
    });
    Some(blocked)
}

impl Blocked {
    /// After a fork, in the parent: let the copy-on-write fault happen, then give each call `round`.
    pub fn feed(&self, round: u64) {
        std::thread::sleep(Duration::from_millis(2));
        let bytes = round.to_ne_bytes();
        // SAFETY: eight bytes from a local array to descriptors this struct owns.
        unsafe {
            write(self.pipe_w, bytes.as_ptr(), 8);
            send(self.sock_w, bytes.as_ptr(), 8, 0);
            write(self.poll_w, bytes.as_ptr(), 8);
        }
    }

    /// Waits until every reader took `round` correctly; the first failure, if any.
    pub fn settle(&self, round: u64) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if let Some(reason) = self.failure.lock().unwrap().clone() {
                return Err(reason);
            }
            if self.done.iter().all(|d| d.load(Ordering::Acquire) >= round) {
                // The readers are back in the kernel before the next fork.
                std::thread::sleep(Duration::from_millis(2));
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(2) {
                let seen: Vec<u64> = self
                    .done
                    .iter()
                    .map(|d| d.load(Ordering::Acquire))
                    .collect();
                return Err(format!("round {round}: the readers stopped at {seen:?}"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

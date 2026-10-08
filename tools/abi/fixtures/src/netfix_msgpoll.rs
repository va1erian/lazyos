//! `netfix`'s `msgpoll` check (issue #667): a `TcpStream` to the host echo
//! server and a Messenger endpoint (`ENDPOINT_FD`, `lazyos_sys::msg::Pollable`)
//! in one `epoll` set. TCP readiness alone, the endpoint alone, both in one
//! wait, and a blocked `epoll_wait(-1)` woken by a TCP echo another thread
//! caused. The Unix-socket twin is the `msgpoll` ABI fixture; this one needs
//! `netd`, so it runs in `tools/net/run.py --netd`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use lazyos_sys::msg::{self, parcel, OwnedHandle, Pollable, EXPIRED_DEADLINE};

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct EpollEvent {
    events: u32,
    data: u64,
}

unsafe extern "C" {
    fn epoll_create1(flags: i32) -> i32;
    fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> i32;
    fn epoll_wait(epfd: i32, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i32;
    fn close(fd: i32) -> i32;
}

const EPOLLIN: u32 = 0x0001;
const EPOLL_CTL_ADD: i32 = 1;
const TCP: u64 = 1;
const ENDPOINT: u64 = 2;
const INTERFACE: u64 = 0x6e65_7466_6978_6d70;

/// An epoll descriptor, closed on drop.
struct Epoll(RawFd);

impl Drop for Epoll {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the descriptor `epoll_create1` returned to us.
        unsafe { close(self.0) };
    }
}

impl Epoll {
    fn new() -> Result<Epoll, String> {
        // SAFETY: `epoll_create1` takes no pointer.
        let fd = unsafe { epoll_create1(0) };
        if fd < 0 {
            return Err(String::from("epoll_create1"));
        }
        Ok(Epoll(fd))
    }

    fn add(&self, fd: RawFd, data: u64) -> Result<(), String> {
        let mut event = EpollEvent {
            events: EPOLLIN,
            data,
        };
        // SAFETY: `event` is a live `struct epoll_event` for the call.
        if unsafe { epoll_ctl(self.0, EPOLL_CTL_ADD, fd, &mut event) } != 0 {
            return Err(format!("epoll_ctl(ADD {fd})"));
        }
        Ok(())
    }

    /// The `data` of the ready interests, sorted (each `EPOLLIN` only).
    fn wait(&self, timeout: i32) -> Result<Vec<u64>, String> {
        let mut out = [EpollEvent::default(); 4];
        // SAFETY: `out` holds four writable events.
        let count = unsafe { epoll_wait(self.0, out.as_mut_ptr(), 4, timeout) };
        if count < 0 {
            return Err(String::from("epoll_wait"));
        }
        let mut ready = Vec::new();
        for event in &out[..count as usize] {
            let (events, data) = (event.events, event.data);
            if events != EPOLLIN {
                return Err(format!("interest {data} reported {events:#x}"));
            }
            ready.push(data);
        }
        ready.sort_unstable();
        Ok(ready)
    }

    /// Wait until the set reports exactly `want` (the echo may take a few
    /// round trips to come back), at most five seconds.
    fn expect(&self, step: &str, want: &[u64]) -> Result<(), String> {
        let give_up = Instant::now() + Duration::from_secs(5);
        loop {
            let ready = self.wait(100)?;
            if ready == want {
                println!("NETFIX:msgpoll:STEP:{step}");
                return Ok(());
            }
            if Instant::now() > give_up {
                return Err(format!(
                    "{step}: epoll reported {ready:?}, expected {want:?}"
                ));
            }
        }
    }
}

fn note() -> libmessenger::Parcel {
    parcel::request(INTERFACE, 1, libmessenger::flags::ONE_WAY, b"tcp".to_vec())
}

/// Read the `n` echoed bytes back from `stream`.
fn drain(stream: &mut TcpStream, n: usize) -> Result<(), String> {
    let mut buf = vec![0u8; n];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("echo read: {e}"))
}

pub fn msgpoll(echo: std::net::SocketAddr) -> Result<String, String> {
    let mut stream = TcpStream::connect(echo).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("{e}"))?;
    let (sender, receiver) = msg::create_pair().map_err(|e| format!("create_pair {e}"))?;
    let sender = OwnedHandle::from_raw(sender);
    let endpoint =
        Pollable::new(OwnedHandle::from_raw(receiver)).map_err(|e| format!("endpoint fd: {e}"))?;
    let set = Epoll::new()?;
    set.add(stream.as_raw_fd(), TCP)?;
    set.add(endpoint.as_raw_fd(), ENDPOINT)?;
    let mut buf = [0u8; 256];
    let take = |buf: &mut [u8]| {
        msg::recv(endpoint.handle(), buf, EXPIRED_DEADLINE)
            .map(drop)
            .map_err(|e| format!("recv {e}"))
    };
    let send = || {
        parcel::send(msg::AsRawHandle::as_raw_handle(&sender), &note())
            .map_err(|e| format!("send {e}"))
    };

    set.expect("quiet", &[])?;
    stream.write_all(b"a").map_err(|e| format!("write: {e}"))?;
    set.expect("tcp_only", &[TCP])?;
    drain(&mut stream, 1)?;
    send()?;
    set.expect("endpoint_only", &[ENDPOINT])?;
    take(&mut buf)?;
    send()?;
    stream.write_all(b"b").map_err(|e| format!("write: {e}"))?;
    set.expect("both", &[TCP, ENDPOINT])?;
    drain(&mut stream, 1)?;
    take(&mut buf)?;
    set.expect("drained", &[])?;

    // Another thread writes through a duplicate of the connection while this
    // one is blocked; the echo coming back is what wakes it.
    let mut writer = stream.try_clone().map_err(|e| format!("try_clone: {e}"))?;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        writer
            .write_all(b"c")
            .map_err(|e| format!("thread write: {e}"))
    });
    let started = Instant::now();
    let woken = set.wait(-1)?;
    thread
        .join()
        .map_err(|_| String::from("writer thread panicked"))??;
    if woken != [TCP] {
        return Err(format!("blocked wait woke with {woken:?}"));
    }
    println!("NETFIX:msgpoll:STEP:blocked_tcp_wake");
    drain(&mut stream, 1)?;
    Ok(format!(
        "blocked wait woke after {} ms",
        started.elapsed().as_millis()
    ))
}

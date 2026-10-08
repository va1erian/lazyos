//! `msgpoll` — a Messenger endpoint and a socket in one `epoll_wait`
//! (issue #667, docs/architecture/endpoint-fd.md): the `ENDPOINT_FD`
//! descriptor through `lazyos_sys::msg::Pollable`, beside a `UnixStream`.
//!
//! Steps, each printing `ABI:msgpoll:STEP:<name>`: the socket alone wakes
//! the set; a queued message alone does; a message sent by another thread
//! wakes a blocked `epoll_wait(-1)`; the drained endpoint goes quiet; closing
//! the peer reads as a hang-up.

mod common;

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use lazyos_sys::msg::{self, parcel, OwnedHandle, Pollable, EXPIRED_DEADLINE};
use libmessenger::{flags, Parcel};

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
}

const EPOLLIN: u32 = 0x0001;
const EPOLLHUP: u32 = 0x0010;
const EPOLL_CTL_ADD: i32 = 1;
const SOCKET: u64 = 1;
const ENDPOINT: u64 = 2;
const NAME: &str = "abi.msgpoll";
const INTERFACE: u64 = 0x6d73_6770_6f6c_6c00;

fn fail(reason: &str) -> ! {
    common::fail("msgpoll", reason)
}

fn add(epfd: RawFd, fd: RawFd, data: u64) {
    let mut event = EpollEvent {
        events: EPOLLIN,
        data,
    };
    // SAFETY: `event` is a live `struct epoll_event` for the call.
    if unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &mut event) } != 0 {
        fail("epoll_ctl(ADD)");
    }
}

/// The `(events, data)` pairs one `epoll_wait` reports, sorted by data.
fn wait(epfd: RawFd, timeout: i32) -> Vec<(u32, u64)> {
    let mut out = [EpollEvent::default(); 4];
    // SAFETY: `out` holds four writable events.
    let count = unsafe { epoll_wait(epfd, out.as_mut_ptr(), 4, timeout) };
    if count < 0 {
        fail("epoll_wait");
    }
    let mut ready: Vec<(u32, u64)> = out[..count as usize]
        .iter()
        .map(|event| (event.events, event.data))
        .collect();
    ready.sort_unstable_by_key(|&(_, data)| data);
    ready
}

fn expect(step: &str, got: Vec<(u32, u64)>, want: &[(u32, u64)]) {
    if got != want {
        fail(&format!(
            "{step}: epoll reported {got:?}, expected {want:?}"
        ));
    }
    println!("ABI:msgpoll:STEP:{step}");
}

/// A one-way message for the watched endpoint.
fn note() -> Parcel {
    parcel::request(INTERFACE, 1, flags::ONE_WAY, b"hi".to_vec())
}

fn main() {
    if !lazyos_sys::detect::on_lazyos() {
        fail("not running on LazyOS");
    }
    // A registered endpoint, so another thread (whose handle table is its
    // own) can resolve it and send.
    let (published, server) = msg::create_pair().unwrap_or_else(|e| fail(&format!("pair {e}")));
    if let Err(code) = parcel::register(NAME, published, &[], &[]) {
        fail(&format!("register {code}"));
    }
    let endpoint = Pollable::new(OwnedHandle::from_raw(server))
        .unwrap_or_else(|e| fail(&format!("endpoint fd: {e}")));
    let (mut near, mut far) = UnixStream::pair().unwrap_or_else(|_| fail("socketpair"));
    // SAFETY: `epoll_create1` takes no pointer.
    let epfd = unsafe { epoll_create1(0) };
    if epfd < 0 {
        fail("epoll_create1");
    }
    add(epfd, near.as_raw_fd(), SOCKET);
    add(epfd, endpoint.as_raw_fd(), ENDPOINT);
    expect("quiet", wait(epfd, 0), &[]);

    far.write_all(b"x").unwrap_or_else(|_| fail("socket write"));
    expect("socket", wait(epfd, 0), &[(EPOLLIN, SOCKET)]);
    let mut byte = [0u8; 1];
    near.read_exact(&mut byte)
        .unwrap_or_else(|_| fail("socket read"));

    let local = parcel::resolve(NAME).unwrap_or_else(|e| fail(&format!("resolve {e}")));
    parcel::send(local, &note()).unwrap_or_else(|e| fail(&format!("send {e}")));
    expect("endpoint", wait(epfd, 0), &[(EPOLLIN, ENDPOINT)]);
    let mut buf = [0u8; 256];
    msg::recv(endpoint.handle(), &mut buf, EXPIRED_DEADLINE)
        .unwrap_or_else(|e| fail(&format!("recv {e}")));
    expect("drained", wait(epfd, 0), &[]);

    // Another thread, its own handle table: resolve, wait, send.
    let sender = std::thread::spawn(|| {
        let handle = parcel::resolve(NAME).unwrap_or_else(|e| fail(&format!("thread resolve {e}")));
        std::thread::sleep(Duration::from_millis(50));
        parcel::send(handle, &note()).unwrap_or_else(|e| fail(&format!("thread send {e}")));
    });
    expect("blocked_wake", wait(epfd, -1), &[(EPOLLIN, ENDPOINT)]);
    sender.join().unwrap_or_else(|_| fail("sender thread"));
    msg::recv(endpoint.handle(), &mut buf, EXPIRED_DEADLINE)
        .unwrap_or_else(|e| fail(&format!("recv {e}")));

    // The peer side goes away: readable with a hang-up.
    let _ = parcel::unregister(NAME);
    msg::close(published).unwrap_or_else(|e| fail(&format!("close {e}")));
    expect("hangup", wait(epfd, 0), &[(EPOLLIN | EPOLLHUP, ENDPOINT)]);
    common::pass("msgpoll");
}

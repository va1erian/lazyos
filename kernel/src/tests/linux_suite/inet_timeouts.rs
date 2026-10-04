//! `SO_RCVTIMEO` and `SO_SNDTIMEO` on `AF_INET` sockets (docs/tls-plan.md
//! §5.4): the option values (round trip, rounding, hostile `timeval`s), a
//! blocking `read`/`recvfrom`/`accept` that gives up with `EAGAIN`, a blocking
//! `write` on a full ring and a blocking `connect` that give up too (`EAGAIN`,
//! `EINPROGRESS`), a timeout of zero that waits for a late peer, a wait a
//! "signal" ends early, and a soak.
//!
//! The suite has no second task, so a peer that acts *during* a wait is the
//! nap hook (`task::harness::set_nap_hook`): the waiting task runs it after
//! every tick it sleeps through, and it delivers bytes or interrupts the wait
//! at the tick [`later`] chose.

use super::inet_core::*;
use super::*;
use crate::ipc::inet::{self, Addr};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

pub(super) const SO_RCVTIMEO: u64 = 20;
pub(super) const SO_SNDTIMEO: u64 = 21;
const EDOM: i64 = 33;

/// `setsockopt(fd, SOL_SOCKET, name, &timeval{sec, usec}, len)`.
pub(super) fn set_timeo_len(fd: u64, name: u64, sec: i64, usec: i64, len: u64) -> u64 {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&sec.to_le_bytes());
    raw[8..].copy_from_slice(&usec.to_le_bytes());
    sys6(54, [fd, 1, name, raw.as_ptr() as u64, len, 0])
}

pub(super) fn set_timeo(fd: u64, name: u64, sec: i64, usec: i64) -> u64 {
    set_timeo_len(fd, name, sec, usec, 16)
}

/// `getsockopt` of a timeout with room for `room` bytes: (result, sec, usec,
/// length written back).
pub(super) fn get_timeo_room(fd: u64, name: u64, room: u32) -> (u64, i64, i64, u32) {
    let mut raw = [0x55u8; 16];
    let mut len = room;
    let r = sys6(
        55,
        [
            fd,
            1,
            name,
            raw.as_mut_ptr() as u64,
            &mut len as *mut u32 as u64,
            0,
        ],
    );
    let word = |at: usize| i64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
    (r, word(0), word(8), len)
}

/// A timeout as `(sec, usec)`; a failed call reads as `(-1, -1)`, which no
/// stored timeout is.
pub(super) fn get_timeo(fd: u64, name: u64) -> (i64, i64) {
    match get_timeo_room(fd, name, 16) {
        (0, sec, usec, 16) => (sec, usec),
        _ => (-1, -1),
    }
}

// ---- a peer that acts later ----------------------------------------------------

const ACT_NONE: u8 = 0;
pub(super) const ACT_DELIVER: u8 = 1;
const ACT_INTERRUPT: u8 = 2;
static ACT: AtomicU8 = AtomicU8::new(ACT_NONE);
static ACT_AT: AtomicU64 = AtomicU64::new(0);
static ACT_ID: AtomicU32 = AtomicU32::new(0);
/// The socket is a datagram one: its bytes need an address in front.
static ACT_DGRAM: AtomicBool = AtomicBool::new(false);
/// The tick the action ran at (0 until it has).
static ACTED_AT: AtomicU64 = AtomicU64::new(0);

/// The payload [`ACT_DELIVER`] writes (a datagram socket gets an address first).
pub(super) const LATE: &[u8] = b"late bytes";

fn act() {
    let now = task::ticks();
    if now < ACT_AT.load(Ordering::Acquire) {
        return;
    }
    match ACT.swap(ACT_NONE, Ordering::AcqRel) {
        ACT_DELIVER => {
            let id = ACT_ID.load(Ordering::Acquire);
            let mut frame = Vec::new();
            if ACT_DGRAM.load(Ordering::Acquire) {
                frame.extend_from_slice(&[10, 0, 2, 3, 0, 53]);
            }
            frame.extend_from_slice(LATE);
            let _ = inet::net_write(id, &frame);
        }
        ACT_INTERRUPT => {
            // What a signal does to a parked task.
            task::harness::interrupt(task::current());
        }
        _ => return,
    }
    ACTED_AT.store(now.max(1), Ordering::Release);
}

/// Arrange for `action` on the socket behind `fd` once `after` ticks passed.
pub(super) fn later(action: u8, fd: u64, after: u64) {
    let dgram = matches!(
        task::fd_clone(fd as usize),
        Some(task::Fd::Inet { ref sock }) if sock.kind() == inet::Kind::Dgram
    );
    let id = id_of(fd);
    ACT_DGRAM.store(dgram, Ordering::Release);
    ACTED_AT.store(0, Ordering::Release);
    ACT_ID.store(id, Ordering::Release);
    ACT_AT.store(task::ticks() + after, Ordering::Release);
    ACT.store(action, Ordering::Release);
    task::harness::set_nap_hook(Some(act));
}

pub(super) fn no_later() {
    ACT.store(ACT_NONE, Ordering::Release);
    task::harness::set_nap_hook(None);
}

/// A connected, blocking TCP socket.
pub(super) fn connected() -> Result<u64, String> {
    let fd = socket(1);
    check!(fd < 16, "socket {fd:#x}");
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    Ok(fd)
}

/// Run `call` and return its result and the ticks it took.
pub(super) fn timed(call: impl FnOnce() -> u64) -> (u64, u64) {
    let before = task::ticks();
    let r = call();
    (r, task::ticks() - before)
}

/// Slack (ticks) allowed past a deadline before the wait counts as too long.
const SLACK: u64 = 15;

// ---- the tests -----------------------------------------------------------------

/// The values: a round trip rounded up to whole ticks, `{0, 0}` as none, and
/// every hostile `timeval` refused or clamped as Linux does.
pub fn inet_timeout_options() -> Result<(), String> {
    inet_fresh()?;
    for fd in [socket(1), socket(SOCK_DGRAM)] {
        for name in [SO_RCVTIMEO, SO_SNDTIMEO] {
            check!(get_timeo(fd, name) == (0, 0), "{name}: no timeout at first");
            check!(set_timeo(fd, name, 1, 500_000) == 0, "{name}: set 1.5 s");
            check!(get_timeo(fd, name) == (1, 500_000), "{name}: round trip");
            check!(set_timeo(fd, name, 0, 1) == 0, "{name}: 1 us");
            check!(
                get_timeo(fd, name) == (0, 10_000),
                "{name}: rounded up to a tick"
            );
            check!(set_timeo(fd, name, 2, 15_000) == 0, "{name}: 2.015 s");
            check!(get_timeo(fd, name) == (2, 20_000), "{name}: rounded up");
            check!(set_timeo(fd, name, 0, 0) == 0, "{name}: none");
            check!(get_timeo(fd, name) == (0, 0), "{name}: none reads back");
            // hostile values
            check!(
                set_timeo(fd, name, 1, 1_000_000) == neg(EDOM),
                "{name}: tv_usec of a whole second"
            );
            check!(
                set_timeo(fd, name, 1, -1) == neg(EDOM),
                "{name}: tv_usec < 0"
            );
            check!(
                set_timeo(fd, name, 0, i64::MIN) == neg(EDOM),
                "{name}: tv_usec = i64::MIN"
            );
            check!(
                get_timeo(fd, name) == (0, 0),
                "{name}: refusals store nothing"
            );
            check!(
                set_timeo(fd, name, i64::MAX, 999_999) == 0,
                "{name}: a huge tv_sec is accepted"
            );
            check!(get_timeo(fd, name) == (0, 0), "{name}: and means none");
            check!(
                set_timeo(fd, name, i64::MAX / 100 - 2, 0) == 0,
                "{name}: the longest finite timeout"
            );
            check!(
                get_timeo(fd, name) == (i64::MAX / 100 - 2, 0),
                "{name}: kept exactly"
            );
            check!(
                set_timeo(fd, name, -1, 0) == 0,
                "{name}: tv_sec < 0 is accepted"
            );
            check!(get_timeo(fd, name) == (0, 0), "{name}: and reads as zero");
            check!(
                set_timeo(fd, name, i64::MIN, 0) == 0,
                "{name}: tv_sec = i64::MIN"
            );
            for len in [0u64, 4, 8, 15] {
                check!(
                    set_timeo_len(fd, name, 1, 0, len) == neg(22),
                    "{name}: optlen {len} is EINVAL"
                );
            }
            check!(
                set_timeo_len(fd, name, 1, 0, 0xFFFF_FFFF) == neg(22),
                "{name}: a negative optlen"
            );
            check!(
                set_timeo_len(fd, name, 3, 0, 64) == 0,
                "{name}: a longer optlen reads 16 bytes"
            );
            check!(get_timeo(fd, name) == (3, 0), "{name}: from the longer one");
            // a short getsockopt buffer gets the prefix and its length
            let (r, sec, usec, len) = get_timeo_room(fd, name, 8);
            check!(
                r == 0 && sec == 3 && usec == 0x5555_5555_5555_5555 && len == 8,
                "{name}: an 8-byte getsockopt: {r:#x} {sec} {usec:#x} {len}"
            );
            check!(
                get_timeo_room(fd, name, 0x8000_0000).0 == neg(22),
                "{name}: a negative getsockopt length"
            );
            check!(set_timeo(fd, name, 0, 0) == 0, "{name}: reset");
        }
        // the two are independent, and another level's option 20 is not one
        check!(set_timeo(fd, SO_RCVTIMEO, 5, 0) == 0, "set recv");
        check!(get_timeo(fd, SO_SNDTIMEO) == (0, 0), "send untouched");
        let one = 1i32;
        check!(
            sys6(54, [fd, 6, 20, &one as *const i32 as u64, 4, 0]) == 0,
            "IPPROTO_TCP option 20 is accepted and ignored"
        );
        check!(get_timeo(fd, SO_RCVTIMEO) == (5, 0), "recv untouched by it");
        check!(set_timeo(fd, 66, 0, 20_000) == 0, "SO_RCVTIMEO_NEW");
        check!(get_timeo(fd, SO_RCVTIMEO) == (0, 20_000), "same timeout");
        // bad pointers
        let strict = crate::user_ptr::set_trust_kernel_pointers(false);
        let set = sys6(54, [fd, 1, SO_RCVTIMEO, 0xdead_0000, 16, 0]);
        let get = sys6(55, [fd, 1, SO_SNDTIMEO, 0xdead_0000, 0xdead_0010, 0]);
        crate::user_ptr::set_trust_kernel_pointers(strict);
        check!(set == neg(14), "setsockopt with a bad pointer -> {set:#x}");
        check!(get == neg(14), "getsockopt with a bad pointer -> {get:#x}");
        check!(get_timeo(fd, SO_RCVTIMEO) == (0, 20_000), "unchanged");
        check!(close(fd) == 0, "close");
    }
    inet_done("inet_timeout_options")
}

/// A blocking read on a silent peer gives up with `EAGAIN` near the timeout;
/// bytes already there come at once; a negative `tv_sec` does not wait; a
/// non-blocking socket is unaffected.
pub fn inet_recv_timeout() -> Result<(), String> {
    inet_fresh()?;
    let fd = connected()?;
    check!(set_timeo(fd, SO_RCVTIMEO, 0, 50_000) == 0, "50 ms");
    let mut buf = [0u8; 32];
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    check!(r == neg(11), "read on a silent peer: {r:#x}");
    check!(
        (5..=5 + SLACK).contains(&took),
        "read waited {took} ticks for 5"
    );
    let (r, took) = timed(|| recvfrom(fd, &mut buf).0);
    check!(r == neg(11), "recvfrom: {r:#x}");
    check!(
        (5..=5 + SLACK).contains(&took),
        "recvfrom waited {took} ticks"
    );
    inet::net_write(id_of(fd), b"ready").map_err(|e| format!("{e}"))?;
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    check!(r == 5 && took <= 1, "data already queued: {r:#x} in {took}");
    check!(set_timeo(fd, SO_RCVTIMEO, -1, 0) == 0, "negative tv_sec");
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    check!(
        r == neg(11) && took <= 1,
        "gives up at once: {r:#x} in {took}"
    );
    check!(set_timeo(fd, SO_RCVTIMEO, 10, 0) == 0, "10 s");
    check!(task::fd_set_status(fd as usize, true), "O_NONBLOCK");
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    check!(
        r == neg(11) && took <= 1,
        "non-blocking is unaffected: {took}"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_recv_timeout")
}

/// A datagram socket's `recvfrom` honours `SO_RCVTIMEO` as well (std's
/// `UdpSocket::set_read_timeout`).
pub fn inet_udp_recv_timeout() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(SOCK_DGRAM);
    check!(sendto(fd, b"q", Some(([10, 0, 2, 3], 53))) == 1, "send");
    let _ = net_take(id_of(fd), 64);
    check!(set_timeo(fd, SO_RCVTIMEO, 0, 40_000) == 0, "40 ms");
    let mut buf = [0u8; 64];
    let (r, took) = timed(|| recvfrom(fd, &mut buf).0);
    check!(r == neg(11), "recvfrom on a silent socket: {r:#x}");
    check!((4..=4 + SLACK).contains(&took), "waited {took} ticks for 4");
    // a datagram that arrives during a longer timeout ends the wait
    check!(set_timeo(fd, SO_RCVTIMEO, 5, 0) == 0, "5 s");
    later(ACT_DELIVER, fd, 3);
    let (r, took) = timed(|| recvfrom(fd, &mut buf).0);
    no_later();
    check!(r == LATE.len() as u64, "the late datagram: {r:#x}");
    check!(&buf[..LATE.len()] == LATE && took < 100, "in {took} ticks");
    check!(close(fd) == 0, "close");
    inet_done("inet_udp_recv_timeout")
}

/// `{0, 0}` means no timeout: a read outlasts the timeout set before it and
/// gets the bytes a peer sends later.
pub fn inet_zero_timeout_waits() -> Result<(), String> {
    inet_fresh()?;
    let fd = connected()?;
    check!(set_timeo(fd, SO_RCVTIMEO, 0, 30_000) == 0, "30 ms");
    check!(set_timeo(fd, SO_RCVTIMEO, 0, 0) == 0, "then none");
    later(ACT_DELIVER, fd, 12);
    let mut buf = [0u8; 32];
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    no_later();
    check!(
        r == LATE.len() as u64,
        "the read got the late bytes: {r:#x}"
    );
    check!(took >= 12, "and waited for them ({took} ticks)");
    check!(ACTED_AT.load(Ordering::Acquire) != 0, "the peer acted");
    check!(close(fd) == 0, "close");
    inet_done("inet_zero_timeout_waits")
}

/// A signal still ends a timed wait at once with `EINTR`.
pub fn inet_timeout_wait_interrupted() -> Result<(), String> {
    inet_fresh()?;
    let fd = connected()?;
    check!(set_timeo(fd, SO_RCVTIMEO, 5, 0) == 0, "5 s");
    later(ACT_INTERRUPT, fd, 3);
    let mut buf = [0u8; 8];
    let (r, took) = timed(|| read_fd(fd, &mut buf));
    no_later();
    check!(r == neg(4), "an interrupted read: {r:#x}");
    check!(
        (3..100).contains(&took),
        "ended by the signal after {took} ticks"
    );
    // the same for a write that waits for space
    fill_ring(fd)?;
    check!(set_timeo(fd, SO_SNDTIMEO, 5, 0) == 0, "5 s send");
    later(ACT_INTERRUPT, fd, 3);
    let (r, took) = timed(|| write_fd(fd, b"more"));
    no_later();
    check!(
        r == neg(4) && took < 100,
        "an interrupted write: {r:#x} {took}"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_timeout_wait_interrupted")
}

/// Write until the application's ring towards `netd` is full.
fn fill_ring(fd: u64) -> Result<usize, String> {
    check!(task::fd_set_status(fd as usize, true), "O_NONBLOCK");
    let chunk = [0x5au8; 4096];
    let mut total = 0usize;
    for _ in 0..64 {
        let r = write_fd(fd, &chunk);
        if r == neg(11) {
            check!(task::fd_set_status(fd as usize, false), "blocking again");
            return Ok(total);
        }
        check!((r as i64) > 0, "filling: {r:#x}");
        total += r as usize;
    }
    Err(format!("the ring took {total} bytes and still had room"))
}

/// A blocking write on a full ring gives up with `EAGAIN`; with some room it
/// returns the bytes it did write.
pub fn inet_send_timeout() -> Result<(), String> {
    inet_fresh()?;
    let fd = connected()?;
    let total = fill_ring(fd)?;
    check!(total == crate::ipc::pipe::SMALL_CAPACITY, "filled {total}");
    check!(set_timeo(fd, SO_SNDTIMEO, 0, 50_000) == 0, "50 ms");
    let (r, took) = timed(|| write_fd(fd, b"x"));
    check!(r == neg(11), "write to a full ring: {r:#x}");
    check!((5..=5 + SLACK).contains(&took), "waited {took} ticks for 5");
    let (r, took) = timed(|| sendto(fd, b"x", None));
    check!(
        r == neg(11) && (5..=5 + SLACK).contains(&took),
        "send: {r:#x} {took}"
    );
    check!(
        net_take(id_of(fd), 100).len() == 100,
        "netd takes 100 bytes"
    );
    let (r, took) = timed(|| write_fd(fd, &[1u8; 4096]));
    check!(r == 100 && took <= 1, "a partial write: {r:#x} in {took}");
    check!(close(fd) == 0, "close");
    inet_done("inet_send_timeout")
}

/// `accept` honours `SO_RCVTIMEO` (`EAGAIN`), and still takes a connection.
pub fn inet_accept_timeout() -> Result<(), String> {
    inet_fresh()?;
    let srv = socket(1);
    check!(bind(srv, [0; 4], 8080) == 0 && listen(srv) == 0, "listen");
    check!(set_timeo(srv, SO_RCVTIMEO, 0, 50_000) == 0, "50 ms");
    let (r, took) = timed(|| sys6(43, [srv, 0, 0, 0, 0, 0]));
    check!(r == neg(11), "accept with nobody calling: {r:#x}");
    check!((5..=5 + SLACK).contains(&took), "waited {took} ticks for 5");
    let peer = Addr {
        ip: [10, 0, 2, 2],
        port: 4000,
    };
    inet::accepted(id_of(srv), peer, Addr::ANY).map_err(|e| format!("{e}"))?;
    let conn = sys6(43, [srv, 0, 0, 0, 0, 0]);
    check!(conn < 16, "accept of a queued connection: {conn:#x}");
    check!(
        get_timeo(conn, SO_RCVTIMEO) == (0, 0),
        "the accepted socket starts with no timeout"
    );
    check!(close(conn) == 0 && close(srv) == 0, "close");
    inet_done("inet_accept_timeout")
}

/// A blocking `connect` waits no longer than `SO_SNDTIMEO`, reports
/// `EINPROGRESS`, and the connection completes (or fails) in the background.
pub fn inet_connect_timeout() -> Result<(), String> {
    inet_fresh()?;
    *inet::RESPONDER.lock() = None; // netd answers only when the test says so
    for refuse in [false, true] {
        set_refuse(refuse);
        let fd = socket(1);
        check!(set_timeo(fd, SO_SNDTIMEO, 0, 50_000) == 0, "50 ms");
        let (r, took) = timed(|| connect(fd, [10, 0, 2, 2], 7));
        check!(r == neg(115), "connect to a silent netd: {r:#x}");
        check!((5..=5 + SLACK).contains(&took), "waited {took} ticks for 5");
        check!(connect(fd, [10, 0, 2, 2], 7) == neg(114), "EALREADY");
        fake_netd();
        let events = poll_one(fd, POLLOUT);
        if refuse {
            check!(events & POLLERR != 0, "the failure is polled: {events:#x}");
            check!(so_error(fd) == 111, "and in SO_ERROR");
        } else {
            check!(events & POLLOUT != 0, "connected: {events:#x}");
            check!(so_error(fd) == 0, "no error");
            check!(write_fd(fd, b"hi") == 2, "usable");
        }
        check!(close(fd) == 0, "close");
    }
    set_refuse(false);
    // a negative tv_sec does not wait at all
    let fd = socket(1);
    check!(set_timeo(fd, SO_SNDTIMEO, -1, 0) == 0, "negative");
    let (r, took) = timed(|| connect(fd, [10, 0, 2, 2], 7));
    check!(
        r == neg(115) && took <= 1,
        "EINPROGRESS at once: {r:#x} {took}"
    );
    fake_netd();
    check!(close(fd) == 0, "close");
    inet_done("inet_connect_timeout")
}

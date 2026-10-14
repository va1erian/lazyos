//! `POLLOUT` on a `SOCK_SEQPACKET` end (issue #863).
//!
//! A seqpacket send is all-or-nothing, so a socket that reports "writable"
//! while only a byte is free sends a `sendmsg` -> `EAGAIN` -> `poll` loop
//! (WebKit's IPC sender) round and round without ever sleeping. The rule
//! under test: `POLLOUT` only while at most a quarter of the ring is in use
//! (Linux's `unix_writable`), so a send of up to three quarters of the ring
//! that follows it never fails with `EAGAIN`.

use super::*;
use crate::ipc::pipe::{Side, SocketPair, POLLOUT};
use crate::task::pollwait;
use crate::task::wait::POLL;
use crate::task::TaskState;
use alloc::collections::VecDeque;

const SOCKET_PAIR: u64 = 53;
const SENDTO: u64 = 44;
const RECVFROM: u64 = 45;
const POLL_CALL: u64 = 7;
const EPOLL_WAIT: u64 = 232;
const EPOLL_CTL: u64 = 233;
const EPOLL_CREATE1: u64 = 291;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLLET: u32 = 1 << 31;
const SOCK_SEQPACKET: u64 = 5;
const MSG_DONTWAIT: u64 = 0x40;
const KIB: usize = 1024;
/// The largest frame `POLLOUT` promises room for: three quarters of the ring.
const PROMISED: usize = 48 * KIB;

fn sys(nr: u64, args: &[u64]) -> u64 {
    let mut all = [0u64; 6];
    all[..args.len()].copy_from_slice(args);
    process::linux::dispatch_args6_for_test(nr, all)
}

fn send(fd: u64, data: &[u8]) -> u64 {
    sys(
        SENDTO,
        &[fd, data.as_ptr() as u64, data.len() as u64, MSG_DONTWAIT],
    )
}

fn recv(fd: u64, buf: &mut [u8]) -> u64 {
    sys(
        RECVFROM,
        &[fd, buf.as_mut_ptr() as u64, buf.len() as u64, MSG_DONTWAIT],
    )
}

/// `poll(fd, events)` with a zero timeout: the `revents`.
fn poll_fd(fd: u64, events: u16) -> Result<u16, String> {
    let mut entry = [0u8; 8];
    entry[..4].copy_from_slice(&(fd as i32).to_le_bytes());
    entry[4..6].copy_from_slice(&events.to_le_bytes());
    let ready = sys(POLL_CALL, &[entry.as_mut_ptr() as u64, 1, 0]);
    check!(ready <= 1, "poll returned {ready:#x}");
    Ok(u16::from_le_bytes([entry[6], entry[7]]))
}

fn pair(kind: u64) -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    check!(
        sys(SOCKET_PAIR, &[1, kind, 0, sv.as_mut_ptr() as u64]) == 0,
        "socketpair"
    );
    Ok((sv[0] as u64, sv[1] as u64))
}

fn epoll_event(events: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&events.to_le_bytes());
    buf
}

/// Whether `epoll_wait(epfd, 0)` reports `EPOLLOUT` right now.
fn epoll_out(epfd: u64) -> bool {
    let mut out = [0u8; 12 * 4];
    let n = sys(EPOLL_WAIT, &[epfd, out.as_mut_ptr() as u64, 4, 0]);
    n == 1 && u32::from_le_bytes([out[0], out[1], out[2], out[3]]) & POLLOUT as u32 != 0
}

/// Whether `poll`, the descriptor-level readiness `select` and `epoll` use,
/// and `epoll_wait` all agree that `fd` is (not) writable.
fn writable_everywhere(fd: u64, epfd: u64) -> Result<bool, String> {
    let by_poll = poll_fd(fd, POLLOUT)? & POLLOUT != 0;
    let by_fd = task::fd_poll(fd as usize, POLLOUT).ok_or("fd_poll: bad fd")? & POLLOUT != 0;
    let by_epoll = epoll_out(epfd);
    check!(
        by_poll == by_fd && by_poll == by_epoll,
        "readiness disagrees: poll {by_poll}, select {by_fd}, epoll {by_epoll}"
    );
    Ok(by_poll)
}

/// The issue's case: a ring filled to 1 KiB from full is not writable, a
/// 4 KiB send would fail, and readiness returns once the reader has drained
/// the ring down to a quarter.
pub fn seqpacket_pollout_needs_room_for_a_frame() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair(SOCK_SEQPACKET)?;
    let epfd = sys(EPOLL_CREATE1, &[0]);
    check!((epfd as i64) > 0, "epoll_create1 {epfd:#x}");
    let interest = epoll_event(POLLOUT as u32);
    check!(
        sys(
            EPOLL_CTL,
            &[epfd, EPOLL_CTL_ADD, a, interest.as_ptr() as u64]
        ) == 0,
        "epoll_ctl"
    );
    check!(
        writable_everywhere(a, epfd)?,
        "an empty pair is not writable"
    );

    let frame = alloc::vec![0x5au8; KIB];
    for i in 0..pipe::CAPACITY / KIB - 1 {
        check!(send(a, &frame) == KIB as u64, "fill frame {i}");
    }
    check!(
        !writable_everywhere(a, epfd)?,
        "writable with 1 KiB free of {}",
        pipe::CAPACITY
    );
    let big = alloc::vec![0u8; 4 * KIB];
    check!(
        send(a, &big) == EAGAIN,
        "a 4 KiB send into 1 KiB of room was accepted"
    );

    // Draining a little is not enough: more than a quarter is still in use
    // (63 KiB queued, 46 read: 17 KiB left).
    let mut buf = alloc::vec![0u8; 64 * KIB];
    for i in 0..46 {
        check!(recv(b, &mut buf) == KIB as u64, "partial drain {i}");
        check!(
            !writable_everywhere(a, epfd)?,
            "writable after draining only {} frames",
            i + 1
        );
    }
    // An edge-triggered interest armed while unwritable stays silent, then
    // reports exactly once when the ring is down to a quarter in use.
    let edge_fd = sys(EPOLL_CREATE1, &[0]);
    check!((edge_fd as i64) > 0, "epoll_create1 (edge) {edge_fd:#x}");
    let edge = epoll_event(POLLOUT as u32 | EPOLLET);
    check!(
        sys(
            EPOLL_CTL,
            &[edge_fd, EPOLL_CTL_ADD, a, edge.as_ptr() as u64]
        ) == 0,
        "epoll_ctl edge"
    );
    check!(!epoll_out(edge_fd), "an edge reported an unwritable socket");
    check!(
        recv(b, &mut buf) == KIB as u64,
        "the drain that reaches 16 KiB"
    );
    check!(writable_everywhere(a, epfd)?, "not writable at a quarter");
    check!(epoll_out(edge_fd), "the edge did not report the room");
    check!(!epoll_out(edge_fd), "the edge reported twice");
    check!(task::fd_close(edge_fd as usize), "close edge epoll");

    // Writable means every size up to three quarters is accepted, with the
    // quarter still queued.
    let largest = alloc::vec![0xc3u8; PROMISED];
    check!(
        send(a, &largest) == PROMISED as u64,
        "a promised frame was refused after POLLOUT"
    );
    check!(!writable_everywhere(a, epfd)?, "writable with a full ring");
    let mut last = 0;
    loop {
        let n = recv(b, &mut buf);
        if n == EAGAIN {
            break;
        }
        last = n;
    }
    check!(last == PROMISED as u64, "the big frame came back as {last}");
    check!(buf[0] == 0xc3 && buf[PROMISED - 1] == 0xc3, "bytes");
    check!(writable_everywhere(a, epfd)?, "not writable after the read");

    // A peer that is gone is POLLERR, never writable.
    check!(task::fd_close(b as usize), "close b");
    let revents = poll_fd(a, POLLOUT)?;
    check!(
        revents & POLLOUT == 0,
        "writable with no reader: {revents:#x}"
    );
    check!(
        task::fd_close(a as usize) && task::fd_close(epfd as usize),
        "close"
    );
    check!(fds_clean() && pipe::Pipe::live() == 0, "leaked");
    Ok(())
}

/// The frame limit counts as "no room" too: 1024 one-byte frames leave the
/// ring nearly empty and still make the socket unwritable, and a send
/// agrees (`EAGAIN`).
pub fn seqpacket_pollout_respects_frame_limit() -> Result<(), String> {
    fresh()?;
    let pair = SocketPair::new_seqpacket().ok_or("pair")?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    for i in 0..pipe::MAX_FRAMES {
        let left = pair.poll(Side::A, POLLOUT) & POLLOUT != 0;
        check!(left, "frame {i}: not writable with {i} bytes queued");
        pair.write_until(Side::A, &[i as u8], true, None)
            .map_err(io_err)?;
    }
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT == 0,
        "writable at the frame limit"
    );
    check!(
        pair.write_until(Side::A, &[0], true, None) == Err(pipe::Error::WouldBlock),
        "a send past the frame limit was accepted"
    );
    let mut one = [0u8; 8];
    for _ in 0..pipe::MAX_FRAMES {
        pair.read_until(Side::B, &mut one, true, None)
            .map_err(io_err)?;
    }
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT != 0,
        "not writable once the frames were read"
    );
    pair.close(Side::A);
    pair.close(Side::B);
    Ok(())
}

/// A byte stream keeps its rule: writable while any byte fits, and a write
/// makes partial progress.
pub fn stream_pollout_still_means_one_byte() -> Result<(), String> {
    fresh()?;
    let pair = SocketPair::new().ok_or("pair")?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    let fill = alloc::vec![1u8; pipe::CAPACITY - 1];
    check!(
        pair.write_until(Side::A, &fill, true, None)
            .map_err(io_err)?
            == fill.len(),
        "fill"
    );
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT != 0,
        "a stream with one byte free is not writable"
    );
    check!(
        pair.write_until(Side::A, &[1, 2, 3], true, None)
            .map_err(io_err)?
            == 1,
        "the partial write"
    );
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT == 0,
        "writable when full"
    );
    pair.close(Side::A);
    pair.close(Side::B);
    Ok(())
}

fn blocked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

/// A poller parked on the pair is woken by the read that makes the socket
/// writable, and finds `POLLOUT` set.
pub fn seqpacket_poller_wakes_when_writable() -> Result<(), String> {
    fresh()?;
    let waiter = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let pair = SocketPair::new_seqpacket().ok_or("pair")?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    let frame = alloc::vec![7u8; 4 * KIB];
    for _ in 0..5 {
        pair.write_until(Side::A, &frame, true, None)
            .map_err(io_err)?;
    }
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT == 0,
        "writable with 20 KiB queued"
    );
    let mut buf = alloc::vec![0u8; 64 * KIB];
    // 20 KiB queued: the poller parks, and the read that leaves 16 KiB (a
    // quarter) must wake it.
    task::harness::switch_current(waiter);
    task::poll_scan_begin();
    pollwait::note_for_test(Some(pair.pipe_keys()));
    task::harness::switch_current(task::KERNEL_TASK);
    POLL.park_ns(waiter, None);
    pair.read_until(Side::B, &mut buf, true, None)
        .map_err(io_err)?;
    check!(
        !blocked(waiter),
        "the read that made room left the poller asleep"
    );
    check!(
        pair.poll(Side::A, POLLOUT) & POLLOUT != 0,
        "the woken poller found no POLLOUT"
    );
    POLL.forget(waiter);
    pair.close(Side::A);
    pair.close(Side::B);
    task::harness::finish(waiter, 0);
    check!(
        task::reap_child().map(|(slot, _)| slot) == Some(waiter),
        "the poller task was not reaped"
    );
    Ok(())
}

/// A tiny deterministic generator (xorshift64*), so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A frame size: mostly WebKit-sized, sometimes up to the promised 48 KiB.
    fn frame_len(&mut self) -> usize {
        let roll = self.next();
        match roll % 8 {
            0 => 1 + (roll >> 8) as usize % PROMISED,
            1..=2 => 1 + (roll >> 8) as usize % (8 * KIB),
            _ => 1 + (roll >> 8) as usize % 300,
        }
    }
}

/// Frame `n`: its number and length up front, then a pattern.
fn make_frame(n: u64, len: usize, out: &mut Vec<u8>) {
    out.clear();
    out.resize(len.max(1), (n % 251) as u8);
    let head = n.to_le_bytes();
    let take = head.len().min(out.len());
    out[..take].copy_from_slice(&head[..take]);
}

/// Stress: a sender that only sends after `POLLOUT` and a reader that is
/// sometimes slow, 100k rounds of random 1 B-48 KiB frames straight on the
/// pipe API. A send after `POLLOUT` must never fail; frames arrive whole and
/// in order; the unwritable state is reached and left many times (so the
/// test is not vacuous).
pub fn seqpacket_poll_then_send_stress() -> Result<(), String> {
    fresh()?;
    let pair = SocketPair::new_seqpacket().ok_or("pair")?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut sent, mut received) = (0u64, 0u64);
    let (mut unwritable, mut eagain_after_pollout) = (0u32, 0u32);
    let mut frame = Vec::new();
    let mut buf = alloc::vec![0u8; 64 * KIB];
    let mut lens = VecDeque::new();
    for round in 0..100_000u32 {
        // The reader takes a few frames now and then: slow on average.
        if rng.next() % 3 == 0 {
            let take = 1 + rng.next() % 3;
            for _ in 0..take {
                match pair.read_until(Side::B, &mut buf, true, None) {
                    Ok(n) => {
                        let want: usize = lens.pop_front().ok_or("a frame from nowhere")?;
                        let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
                        check!(
                            n == want && seq == received,
                            "round {round}: frame {seq} of {n} bytes, wanted {received} of {want}"
                        );
                        received += 1;
                    }
                    Err(pipe::Error::WouldBlock) => break,
                    Err(error) => return Err(io_err(error)),
                }
            }
        }
        if pair.poll(Side::A, POLLOUT) & POLLOUT == 0 {
            unwritable += 1;
            continue;
        }
        let len = rng.frame_len().max(8);
        make_frame(sent, len, &mut frame);
        match pair.write_until(Side::A, &frame, true, None) {
            Ok(n) if n == frame.len() => {
                lens.push_back(n);
                sent += 1;
            }
            Err(pipe::Error::WouldBlock) => eagain_after_pollout += 1,
            other => return Err(format!("round {round}: send of {len}: {other:?}")),
        }
    }
    check!(
        eagain_after_pollout == 0,
        "{eagain_after_pollout} sends failed with EAGAIN right after POLLOUT"
    );
    check!(
        unwritable > 5_000 && sent > 5_000,
        "vacuous run: {unwritable} unwritable rounds, {sent} sent"
    );
    while let Ok(n) = pair.read_until(Side::B, &mut buf, true, None) {
        let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
        check!(seq == received && Some(n) == lens.pop_front(), "drain");
        received += 1;
    }
    check!(received == sent, "{received} received of {sent} sent");
    pair.close(Side::A);
    pair.close(Side::B);
    drop(pair);
    check!(pipe::Pipe::live() == 0, "pipes leaked");
    Ok(())
}

/// The same loop through the system calls (`poll`, `sendto`, `recvfrom`), the
/// way WebKit's `sendOutputMessage` and its receiver run it.
pub fn seqpacket_poll_then_sendto_stress() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair(SOCK_SEQPACKET)?;
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let (mut sent, mut received, mut unwritable) = (0u64, 0u64, 0u32);
    let mut frame = Vec::new();
    let mut buf = alloc::vec![0u8; 64 * KIB];
    for round in 0..20_000u32 {
        if rng.next() % 3 == 0 {
            while recv(b, &mut buf) != EAGAIN {
                let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
                check!(
                    seq == received,
                    "round {round}: frame {seq}, want {received}"
                );
                received += 1;
                if rng.next() % 2 == 0 {
                    break;
                }
            }
        }
        if poll_fd(a, POLLOUT)? & POLLOUT == 0 {
            unwritable += 1;
            continue;
        }
        let len = rng.frame_len().max(8);
        make_frame(sent, len, &mut frame);
        let ret = send(a, &frame);
        check!(
            ret == frame.len() as u64,
            "round {round}: sendto of {len} after POLLOUT returned {ret:#x}"
        );
        sent += 1;
    }
    loop {
        let n = recv(b, &mut buf);
        if n == EAGAIN {
            break;
        }
        check!(n <= buf.len() as u64, "final drain: recv returned {n:#x}");
        let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
        check!(seq == received, "final drain: frame {seq}, want {received}");
        received += 1;
    }
    check!(received == sent, "{received} received of {sent} sent");
    check!(unwritable > 1_000, "vacuous run: {unwritable}");
    check!(
        task::fd_close(a as usize) && task::fd_close(b as usize),
        "close"
    );
    check!(fds_clean() && pipe::Pipe::live() == 0, "leaked");
    Ok(())
}

//! The edge-trigger freshness counter of a socketpair end (issue #868).
//!
//! `SocketPair::poll_gen` once combined the two direction counters with an
//! XOR, which repeats when both rise by the same amount between two checks
//! (`1 ^ 1 == 2 ^ 2`). An `EPOLLET` interest whose `revents` did not change
//! (already readable and writable) then missed the new data.

use super::*;
use crate::ipc::pipe::{Side, SocketPair, POLLIN, POLLOUT};

const SOCKET_PAIR: u64 = 53;
const SENDTO: u64 = 44;
const RECVFROM: u64 = 45;
const EPOLL_WAIT: u64 = 232;
const EPOLL_CTL: u64 = 233;
const EPOLL_CREATE1: u64 = 291;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLLET: u32 = 1 << 31;
const MSG_DONTWAIT: u64 = 0x40;

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

/// The number of events `epoll_wait(epfd, 0)` returns right now.
fn epoll_events(epfd: u64) -> u64 {
    let mut out = [0u8; 12 * 4];
    sys(EPOLL_WAIT, &[epfd, out.as_mut_ptr() as u64, 4, 0])
}

/// The issue's case through the system calls. `a` is readable and writable
/// and an edge interest watches it; then `b` writes one byte to `a` and reads
/// one byte `a` sent (both of `a`'s counters rise by one, from equal values):
/// the edge must report the new data.
pub fn socketpair_edge_survives_equal_counter_steps() -> Result<(), String> {
    fresh()?;
    let mut sv = [0i32; 2];
    check!(
        sys(SOCKET_PAIR, &[1, SOCK_STREAM, 0, sv.as_mut_ptr() as u64]) == 0,
        "socketpair"
    );
    let (a, b) = (sv[0] as u64, sv[1] as u64);
    let mut byte = [0u8; 1];
    // `a` sees one write by `b` and `b` one read of what `a` sent: the two
    // counters of `a` are equal (1 and 1) and `a` is readable and writable.
    check!(send(b, b"x") == 1, "b writes");
    check!(send(a, b"yz") == 2, "a writes");
    check!(recv(b, &mut byte) == 1, "b reads");

    let epfd = sys(EPOLL_CREATE1, &[0]);
    check!((epfd as i64) > 0, "epoll_create1 {epfd:#x}");
    let edge = {
        let mut buf = [0u8; 12];
        buf[..4].copy_from_slice(&(POLLIN as u32 | POLLOUT as u32 | EPOLLET).to_le_bytes());
        buf
    };
    check!(
        sys(EPOLL_CTL, &[epfd, EPOLL_CTL_ADD, a, edge.as_ptr() as u64]) == 0,
        "epoll_ctl"
    );
    check!(
        epoll_events(epfd) == 1,
        "no initial edge for a ready socket"
    );
    check!(epoll_events(epfd) == 0, "the edge reported twice");

    // The same step on both counters: revents stay IN|OUT.
    check!(send(b, b"x") == 1, "b writes again");
    check!(recv(b, &mut byte) == 1, "b reads again");
    check!(
        epoll_events(epfd) == 1,
        "the edge for new data was lost (both counters rose by one)"
    );
    check!(epoll_events(epfd) == 0, "the edge reported twice");

    check!(
        task::fd_close(a as usize) && task::fd_close(b as usize) && task::fd_close(epfd as usize),
        "close"
    );
    check!(fds_clean() && pipe::Pipe::live() == 0, "leaked");
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
}

/// Stress: batches of random operations on a pair while end `A` stays
/// readable and writable. `A`'s counter must differ after a batch exactly
/// when the batch held an operation that changes it (`B` writing, or `B`
/// reading what `A` sent); batches are short so both counters often rise by
/// the same amount, the case that fooled the XOR.
pub fn socketpair_poll_gen_changes_with_every_event() -> Result<(), String> {
    fresh()?;
    let pair = SocketPair::new().ok_or("pair")?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut buf = [0u8; 1];
    // Seed `A`'s inbox so it is readable throughout.
    for _ in 0..8 {
        pair.write_until(Side::B, &[0], true, None)
            .map_err(io_err)?;
    }
    let (mut last_revents, mut last_gen) = pair.poll_gen(Side::A, POLLIN | POLLOUT);
    check!(
        last_revents == POLLIN | POLLOUT,
        "not ready: {last_revents:#x}"
    );
    let (mut changed_batches, mut quiet_batches) = (0u32, 0u32);
    for round in 0..200_000u32 {
        let mut relevant = false;
        for _ in 0..1 + rng.next() % 4 {
            match rng.next() % 4 {
                // `B` writes to `A`: A's inbox gains a byte. Relevant.
                0 => {
                    if pair.write_until(Side::B, &[1], true, None).is_ok() {
                        relevant = true;
                    }
                }
                // `B` reads what `A` sent: A's outbox gains room. Relevant.
                1 => {
                    if pair.read_until(Side::B, &mut buf, true, None).is_ok() {
                        relevant = true;
                    }
                }
                // `A` sends: not an event of A's own counter.
                2 => {
                    let _ = pair.write_until(Side::A, &[2], true, None);
                }
                // `A` reads: not an event of A's own counter.
                _ => {
                    let _ = pair.read_until(Side::A, &mut buf, true, None);
                }
            }
        }
        // Keep `A` readable (a refill is `B` writing: relevant).
        if pair.poll(Side::A, POLLIN) & POLLIN == 0 {
            pair.write_until(Side::B, &[3], true, None)
                .map_err(io_err)?;
            relevant = true;
        }
        let (revents, gen) = pair.poll_gen(Side::A, POLLIN | POLLOUT);
        check!(
            revents == last_revents,
            "round {round}: revents {revents:#x}, wanted {last_revents:#x}"
        );
        check!(
            (gen != last_gen) == relevant,
            "round {round}: counter {last_gen} -> {gen} but an event happened: {relevant}"
        );
        if relevant {
            changed_batches += 1;
        } else {
            quiet_batches += 1;
        }
        last_revents = revents;
        last_gen = gen;
    }
    check!(
        changed_batches > 50_000 && quiet_batches > 1_000,
        "vacuous run: {changed_batches} event batches, {quiet_batches} quiet"
    );
    pair.close(Side::A);
    pair.close(Side::B);
    drop(pair);
    check!(pipe::Pipe::live() == 0, "pipes leaked");
    Ok(())
}

//! `AF_INET` under sustained load (docs/networking-plan.md N5): thousands of
//! connections and datagrams, a megabyte through the rings each way, a `netd`
//! restart with sockets open, and a long random-call sequence. What these look
//! for is a leaked slot, ring or descriptor, a broken bound, or a byte out of
//! place.

use super::inet_core::*;
use super::*;
use crate::ipc::inet::{self, Io};

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn pattern(round: u32, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (round as u8) ^ (i as u8).wrapping_mul(7))
        .collect()
}

/// Connect, exchange bytes, close, 3 000 times: every slot, ring and
/// descriptor comes back.
pub fn inet_connection_soak() -> Result<(), String> {
    inet_fresh()?;
    for round in 0..3_000u32 {
        let fd = socket(1);
        check!(fd < 16, "round {round}: socket {fd:#x}");
        check!(connect(fd, [10, 0, 2, 2], 7) == 0, "round {round}: connect");
        let id = id_of(fd);
        let out = pattern(round, 1 + (round as usize * 37) % 1500);
        check!(
            write_fd(fd, &out) == out.len() as u64,
            "round {round}: write"
        );
        check!(net_take(id, 4096) == out, "round {round}: netd read");
        let back = pattern(round ^ 0x55, 1 + (round as usize * 91) % 1500);
        check!(
            matches!(inet::net_write(id, &back), Ok(Io::Data(n)) if n == back.len()),
            "round {round}: netd write"
        );
        let mut buf = vec![0u8; 2048];
        let n = read_fd(fd, &mut buf);
        check!(
            n == back.len() as u64 && buf[..back.len()] == back[..],
            "round {round}: read"
        );
        check!(close(fd) == 0, "round {round}: close");
        fake_netd();
    }
    inet_done("inet_connection_soak")
}

/// 5 000 datagrams of every length: boundaries, addresses and counts hold.
pub fn inet_datagram_soak() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(SOCK_DGRAM);
    let id;
    check!(
        sendto(fd, b"x", Some(([10, 0, 2, 3], 1))) == 1,
        "first send"
    );
    id = id_of(fd);
    check!(net_take(id, 64).len() == 7, "first frame");
    let mut buf = vec![0u8; 2048];
    for round in 0..5_000u32 {
        let len = (round as usize * 13) % 1473;
        let port = 1 + (round % 60_000) as u16;
        let payload = pattern(round, len);
        check!(
            sendto(fd, &payload, Some(([10, 0, 2, 3], port))) == len as u64,
            "round {round}: send"
        );
        let frame = net_take(id, 2048);
        check!(
            frame.len() == 6 + len
                && frame[4..6] == port.to_be_bytes()
                && frame[6..] == payload[..],
            "round {round}: frame"
        );
        let mut reply = vec![192, 168, round as u8, 9];
        reply.extend_from_slice(&port.to_be_bytes());
        reply.extend_from_slice(&payload);
        check!(
            matches!(inet::net_write(id, &reply), Ok(Io::Data(n)) if n == reply.len()),
            "round {round}: deliver"
        );
        let (n, from) = recvfrom(fd, &mut buf);
        check!(
            n == len as u64 && buf[..len] == payload[..],
            "round {round}: recv {n:#x}"
        );
        check!(
            from == ([192, 168, round as u8, 9], port),
            "round {round}: source"
        );
    }
    check!(close(fd) == 0, "close");
    inet_done("inet_datagram_soak")
}

/// A megabyte each way through the 32 KiB rings, byte for byte, with the
/// application never blocking.
pub fn inet_bulk_transfer() -> Result<(), String> {
    inet_fresh()?;
    let fd = sys6(41, [AF_INET, 1 | SOCK_NONBLOCK, 0, 0, 0, 0]);
    *inet::RESPONDER.lock() = None;
    check!(connect(fd, [10, 0, 2, 2], 7) == neg(115), "EINPROGRESS");
    fake_netd();
    let id = id_of(fd);
    let total = 1 << 20;
    let data = pattern(3, total);
    let (mut sent, mut at_netd, mut echoed, mut got) = (0usize, 0usize, 0usize, 0usize);
    let mut buf = vec![0u8; 8192];
    let mut spins = 0;
    while got < total {
        spins += 1;
        check!(
            spins < 100_000,
            "stalled at {sent}/{at_netd}/{echoed}/{got}"
        );
        if sent < total {
            let n = write_fd(fd, &data[sent..(sent + 6000).min(total)]);
            if n != neg(11) {
                check!((n as i64) > 0, "write returned {n:#x}");
                sent += n as usize;
            }
        }
        let chunk = net_take(id, 5000);
        check!(
            chunk[..] == data[at_netd..at_netd + chunk.len()],
            "the bytes netd read differ at {at_netd}"
        );
        at_netd += chunk.len();
        // netd echoes what it has read, as far as the receive ring allows
        while echoed < at_netd {
            match inet::net_write(id, &data[echoed..at_netd.min(echoed + 4000)]) {
                Ok(Io::Data(n)) => echoed += n,
                _ => break,
            }
        }
        let n = read_fd(fd, &mut buf);
        if n != neg(11) {
            check!((n as i64) > 0, "read returned {n:#x}");
            check!(
                buf[..n as usize] == data[got..got + n as usize],
                "the echo differs at {got}"
            );
            got += n as usize;
        }
    }
    check!(
        sent == total && at_netd == total && echoed == total,
        "all moved"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_bulk_transfer")
}

/// An application that writes and closes at once: `netd` still reads every
/// byte, then the end of stream.
pub fn inet_close_after_write_loses_nothing() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let id = id_of(fd);
    let data = pattern(9, 20_000);
    let mut sent = 0;
    while sent < data.len() {
        let n = write_fd(fd, &data[sent..]);
        check!((n as i64) > 0, "write returned {n:#x} after {sent} bytes");
        sent += n as usize;
    }
    check!(close(fd) == 0, "close");
    let mut got = Vec::new();
    loop {
        let mut buf = vec![0u8; 4096];
        match inet::net_read(id, &mut buf) {
            Ok(Io::Data(n)) => got.extend_from_slice(&buf[..n]),
            Ok(Io::Eof) => break,
            other => return Err(format!("unexpected {other:?}")),
        }
    }
    check!(
        got == data,
        "all {} bytes arrived, then the end",
        data.len()
    );
    inet_done("inet_close_after_write")
}

/// A restarted `netd` finds the old sockets gone; their applications read end
/// of stream and get `EPIPE`, and can still close.
pub fn inet_netd_restart() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    let udp = socket(SOCK_DGRAM);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    check!(bind(udp, [0; 4], 5000) == 0, "bind");
    inet::attach(task::current()); // the new netd
    check!(
        inet::live_count() == 0 && inet::queued_count() == 0,
        "the table was cleared"
    );
    let mut buf = [0u8; 8];
    check!(read_fd(fd, &mut buf) == 0, "end of stream");
    check!(write_fd(fd, b"x") == neg(32), "EPIPE");
    check!(poll_one(fd, POLLIN) & POLLHUP != 0, "hangup");
    check!(
        close(fd) == 0 && close(udp) == 0,
        "the old descriptors close"
    );
    check!(
        inet::queued_count() == 0,
        "and queue nothing for the new netd"
    );
    // sockets made after the restart work
    let fresh = socket(1);
    check!(connect(fresh, [10, 0, 2, 2], 7) == 0, "a new connection");
    check!(close(fresh) == 0, "close");
    inet_done("inet_netd_restart")
}

/// Random calls on a few sockets, answered at random: nothing panics, the
/// table stays bounded, and everything is returned at the end.
pub fn inet_random_calls() -> Result<(), String> {
    inet_fresh()?;
    // Every socket is non-blocking, so nothing waits for a scheduler that is
    // not there; the fake `netd` answers bind and listen on the spot.
    for seed in 1..=6u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut fds: Vec<u64> = Vec::new();
        let mut buf = vec![0u8; 3000];
        for step in 0..2_000u32 {
            let pick = |rng: &mut Rng, fds: &Vec<u64>| -> u64 {
                if fds.is_empty() || rng.below(10) == 0 {
                    // Never 0-2: those are the terminal, and a read there would wait for a key.
                    3 + rng.below(17)
                } else {
                    fds[rng.below(fds.len() as u64) as usize]
                }
            };
            let fd = pick(&mut rng, &fds);
            let port = (rng.below(70_000)) as u16;
            let len = rng.below(2000) as usize;
            let payload = pattern(step, len);
            match rng.below(16) {
                0 | 1 => {
                    let kind = 1 + rng.below(2);
                    let new = sys6(41, [AF_INET, kind | SOCK_NONBLOCK, 0, 0, 0, 0]);
                    if (new as i64) > 0 {
                        fds.push(new);
                    }
                }
                2 => {
                    let _ = connect(fd, [10, 0, 2, 2], port);
                }
                3 => {
                    let _ = sendto(fd, &payload, Some(([10, 0, 2, 3], port)));
                }
                4 => {
                    let _ = write_fd(fd, &payload);
                }
                5 => {
                    let _ = read_fd(fd, &mut buf);
                }
                6 => {
                    let _ = recvfrom(fd, &mut buf);
                }
                7 => {
                    let _ = sys6(48, [fd, rng.below(4), 0, 0, 0, 0]);
                }
                8 => {
                    let _ = poll_one(fd, POLLIN | POLLOUT);
                }
                9 => {
                    let _ = so_error_raw(fd);
                }
                10 => {
                    let new = sys6(32, [fd, 0, 0, 0, 0, 0]);
                    if (new as i64) > 0 {
                        fds.push(new);
                    }
                }
                11 | 12 => {
                    if !fds.is_empty() {
                        let at = rng.below(fds.len() as u64) as usize;
                        let victim = fds.swap_remove(at);
                        let _ = close(victim);
                    }
                }
                13 => fake_netd(),
                14 => {
                    let id = id_of(fd);
                    if id != 0 {
                        let _ = inet::net_write(id, &payload);
                        let _ = net_take(id, 2048);
                    }
                }
                _ => {
                    let id = id_of(fd);
                    if id != 0 && rng.below(4) == 0 {
                        let _ = inet::net_eof(id);
                    }
                }
            }
            check!(
                inet::live_count() <= inet::MAX_SOCKETS,
                "seed {seed} step {step}: table over its bound"
            );
            check!(
                pipe::Pipe::live_small() <= pipe::MAX_SMALL_PIPES,
                "seed {seed} step {step}: rings over their bound"
            );
        }
        for fd in fds {
            let _ = close(fd);
        }
        fake_netd();
        check!(fds_clean(), "seed {seed}: descriptors left");
        check!(
            inet::live_count() == 0,
            "seed {seed}: {} sockets left",
            inet::live_count()
        );
    }
    inet_done("inet_random_calls")
}

fn so_error_raw(fd: u64) -> u64 {
    let mut value = 0i32;
    let mut len = 4u32;
    sys6(
        55,
        [
            fd,
            1,
            4,
            &mut value as *mut i32 as u64,
            &mut len as *mut u32 as u64,
            0,
        ],
    )
}

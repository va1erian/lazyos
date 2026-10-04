//! The `AF_INET` pump's doorbell (docs/performance-plan.md P4.1): exactly
//! the application's actions ring it, never `netd`'s own, and a `netd` that
//! passes only when it rang (or when its last pass moved bytes) still moves
//! every byte of many sockets at once, in order, past a slow reader.
//!
//! The parked side (a `netd` asleep in `wait_any` and woken by the bell) is
//! `waitset_suite::inet_doorbell`; here the fake `netd` is the test task and
//! looks at the bell with [`rang`].

use super::inet_core::*;
use super::*;
use crate::ipc::inet::{self, bell, Io};

/// Whether the bell rang since the last look (consumes it, as `netd`'s wait
/// does, and leaves nothing armed).
fn rang() -> bool {
    match bell::arm(task::current()) {
        Ok(true) => true,
        Ok(false) => {
            bell::disarm(task::current());
            false
        }
        Err(()) => panic!("the test task is not the attached netd"),
    }
}

/// Each application action that `netd` must see rings once; what `netd`
/// itself does, and application writes into a ring it has not drained, do
/// not.
pub fn inet_bell_rings_for_the_application() -> Result<(), String> {
    inet_fresh()?;
    let _ = rang();
    let fd = socket(1);
    check!(!rang(), "creating a socket rang");
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    check!(rang(), "a queued connect did not ring");
    let id = id_of(fd);

    check!(write_fd(fd, b"first") == 5, "write");
    check!(rang(), "a write into an empty ring did not ring");
    check!(write_fd(fd, b"second") == 6, "write");
    check!(!rang(), "a write into a ring netd has not drained rang");
    check!(net_take(id, 64) == b"firstsecond", "netd read");
    check!(!rang(), "netd's own read rang");
    check!(write_fd(fd, b"third") == 5, "write");
    check!(rang(), "a write after netd drained the ring did not ring");
    net_take(id, 64);

    // netd fills the receive ring: its writes never ring.
    let chunk = [0x5Au8; 1000];
    let mut filled = 0;
    while let Ok(Io::Data(n)) = inet::net_write(id, &chunk) {
        filled += n;
    }
    check!(filled == pipe::SMALL_CAPACITY, "filled {filled}");
    check!(!rang(), "netd's own writes rang");
    let mut buf = vec![0u8; 4096];
    check!(read_fd(fd, &mut buf[..100]) == 100, "read from full");
    check!(rang(), "a read from a full ring did not ring");
    check!(read_fd(fd, &mut buf[..3000]) == 3000, "read");
    check!(rang(), "a read from a nearly full ring did not ring");
    check!(read_fd(fd, &mut buf) == 4096, "read");
    check!(!rang(), "a read from a ring with room rang");

    check!(sys6(48, [fd, 1, 0, 0, 0, 0]) == 0, "shutdown(SHUT_WR)");
    check!(rang(), "shutdown did not ring");
    check!(close(fd) == 0, "close");
    check!(rang(), "close did not ring");
    check!(!rang(), "a ring was counted twice");

    // A socket that never asked netd for anything still tells it it closed.
    let udp = socket(SOCK_DGRAM);
    check!(!rang(), "socket() rang");
    check!(close(udp) == 0, "close udp");
    check!(rang(), "closing an unused socket did not ring");
    inet_done("inet_bell_rings_for_the_application")
}

/// The deterministic stream of socket `sock`.
fn byte(sock: usize, at: usize) -> u8 {
    (at as u8).wrapping_mul(31) ^ (at >> 9) as u8 ^ (sock as u8).wrapping_mul(97)
}

struct Conn {
    fd: u64,
    id: u32,
    /// Bytes the application wrote, `netd` took, `netd` echoed, the
    /// application read back.
    sent: usize,
    taken: usize,
    echoed: usize,
    got: usize,
}

/// Soak: 32 sockets at once, 256 KiB each way through each, a slow reader
/// (777 bytes a turn) and a `netd` that passes only when the bell rang or
/// its last pass moved bytes, exactly the service's rule. A missed ring
/// stalls the loop; a lost, duplicated or reordered byte fails the check.
pub fn inet_bell_slow_reader_soak() -> Result<(), String> {
    const SOCKETS: usize = 32;
    const TOTAL: usize = 256 * 1024;
    inet_fresh()?;
    let mut conns = Vec::new();
    for _ in 0..SOCKETS {
        let fd = sys6(41, [AF_INET, 1 | SOCK_NONBLOCK, 0, 0, 0, 0]);
        check!(fd < 64, "socket {fd:#x}");
        let _ = connect(fd, [10, 0, 2, 2], 7);
        fake_netd();
        conns.push(Conn {
            fd,
            id: id_of(fd),
            sent: 0,
            taken: 0,
            echoed: 0,
            got: 0,
        });
    }
    let _ = rang();
    let (mut passes, mut idle, mut moved) = (0u64, 0u32, true);
    let mut out = vec![0u8; 64 * 1024];
    let mut buf = vec![0u8; 64 * 1024];
    while conns.iter().any(|c| c.got < TOTAL) {
        let mut progress = false;
        // The applications: one write and one slow read each.
        for (n, c) in conns.iter_mut().enumerate() {
            if c.sent < TOTAL {
                let len = (TOTAL - c.sent).min(24 * 1024 + n * 113);
                for (i, b) in out[..len].iter_mut().enumerate() {
                    *b = byte(n, c.sent + i);
                }
                let w = write_fd(c.fd, &out[..len]);
                if w != neg(11) {
                    check!((w as i64) > 0, "socket {n}: write {w:#x}");
                    c.sent += w as usize;
                    progress = true;
                }
            }
            let r = read_fd(c.fd, &mut buf[..777]);
            if r != neg(11) {
                check!((r as i64) > 0, "socket {n}: read {r:#x}");
                let r = r as usize;
                let at = (0..r).find(|&i| buf[i] != byte(n, c.got + i));
                check!(
                    at.is_none(),
                    "socket {n}: byte {} differs",
                    c.got + at.unwrap_or(0)
                );
                c.got += r;
                progress = true;
            }
        }
        // netd: only when rung, or when its last pass moved bytes.
        if rang() || moved {
            passes += 1;
            moved = false;
            for (n, c) in conns.iter_mut().enumerate() {
                if let Ok(Io::Data(k)) = inet::net_read(c.id, &mut buf) {
                    let at = (0..k).find(|&i| buf[i] != byte(n, c.taken + i));
                    check!(
                        at.is_none(),
                        "socket {n}: netd saw byte {} differ",
                        c.taken + at.unwrap_or(0)
                    );
                    c.taken += k;
                    moved = true;
                }
                while c.echoed < c.taken {
                    let end = c.taken.min(c.echoed + 16 * 1024);
                    for (i, b) in out[..end - c.echoed].iter_mut().enumerate() {
                        *b = byte(n, c.echoed + i);
                    }
                    match inet::net_write(c.id, &out[..end - c.echoed]) {
                        Ok(Io::Data(k)) => {
                            c.echoed += k;
                            moved = true;
                        }
                        _ => break,
                    }
                }
            }
            progress |= moved;
        }
        idle = if progress { 0 } else { idle + 1 };
        check!(
            idle < 4,
            "stalled: {:?}",
            conns
                .iter()
                .map(|c| (c.sent, c.taken, c.echoed, c.got))
                .collect::<Vec<_>>()
        );
    }
    for c in &conns {
        check!(close(c.fd) == 0, "close");
    }
    serial_println!(
        "TEST:linux_inet_bell_slow_reader_soak:INFO:sockets={SOCKETS} bytes_each_way={TOTAL} netd_passes={passes} rings={}",
        bell::rings()
    );
    inet_done("inet_bell_slow_reader_soak")
}

/// One `write` or `read` of a stream socket moves up to a whole ring
/// (P4.4; it was 4 KiB a call): a 200 KiB write is taken whole, a write past
/// the ring's size is short at exactly its free space, a read with room for
/// everything returns everything queued, and the bytes are in order.
pub fn inet_large_stream_calls() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let id = id_of(fd);
    let big: Vec<u8> = (0..300 * 1024).map(|i| byte(3, i)).collect();
    check!(
        write_fd(fd, &big[..200 * 1024]) == 200 * 1024,
        "a 200 KiB write was not taken whole"
    );
    let n = write_fd(fd, &big[200 * 1024..]);
    check!(
        n == (pipe::SMALL_CAPACITY - 200 * 1024) as u64,
        "a write past the ring took {n:#x}"
    );
    let mut got = vec![0u8; pipe::SMALL_CAPACITY];
    match inet::net_read(id, &mut got) {
        Ok(Io::Data(k)) => {
            check!(k == pipe::SMALL_CAPACITY, "netd read {k}");
            check!(got == big[..k], "the bytes netd read differ");
        }
        other => return Err(format!("netd read {other:?}")),
    }
    check!(
        matches!(inet::net_write(id, &big), Ok(Io::Data(k)) if k == pipe::SMALL_CAPACITY),
        "netd fill"
    );
    let mut buf = vec![0u8; 512 * 1024];
    let r = read_fd(fd, &mut buf);
    check!(
        r == pipe::SMALL_CAPACITY as u64
            && buf[..pipe::SMALL_CAPACITY] == big[..pipe::SMALL_CAPACITY],
        "a large read returned {r:#x}"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_large_stream_calls")
}

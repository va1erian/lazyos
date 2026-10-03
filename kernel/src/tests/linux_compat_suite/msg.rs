//! `MSG_PEEK`, `MSG_DONTWAIT`, `MSG_WAITALL` and `sendmsg`/`recvmsg` on
//! `AF_UNIX` stream and seqpacket pairs.

use super::*;

const SENDTO: u64 = 44;
const RECVFROM: u64 = 45;
const SENDMSG: u64 = 46;
const RECVMSG: u64 = 47;
const MSG_PEEK: u64 = 0x2;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_WAITALL: u64 = 0x100;
const MSG_OOB: u64 = 0x1;

fn send(fd: u64, data: &[u8], flags: u64) -> u64 {
    sys(
        SENDTO,
        &[fd, data.as_ptr() as u64, data.len() as u64, flags, 0, 0],
    )
}

fn recv(fd: u64, buf: &mut [u8], flags: u64) -> u64 {
    sys(
        RECVFROM,
        &[fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags, 0, 0],
    )
}

/// A peek leaves the bytes for the next read; `MSG_DONTWAIT` on an empty
/// blocking socket is `EAGAIN` instead of a hang.
pub fn msg_peek_and_dontwait() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair()?;
    let mut buf = [0u8; 16];
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "empty DONTWAIT did not EAGAIN"
    );
    check!(send(a, b"hello", 0) == 5, "send");
    check!(
        recv(b, &mut buf[..3], MSG_PEEK) == 3 && &buf[..3] == b"hel",
        "peek"
    );
    check!(
        recv(b, &mut buf, MSG_PEEK | MSG_DONTWAIT) == 5,
        "second peek"
    );
    let mut count = 0i32;
    check!(
        sys(16, &[b, 0x541B, &mut count as *mut i32 as u64]) == 0 && count == 5,
        "FIONREAD {count}"
    );
    check!(
        recv(b, &mut buf, 0) == 5 && &buf[..5] == b"hello",
        "the read after peeks"
    );
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "the bytes were read twice"
    );
    // WAITALL with everything already queued returns the whole buffer.
    check!(send(a, b"0123456789", 0) == 10, "send 10");
    check!(recv(b, &mut buf[..10], MSG_WAITALL) == 10, "WAITALL");
    let (seq_a, seq_b) = {
        let mut sv = [0i32; 2];
        check!(
            sys(53, &[1, 5, 0, sv.as_mut_ptr() as u64]) == 0,
            "seqpacket pair"
        );
        (sv[0] as u64, sv[1] as u64)
    };
    check!(
        send(seq_a, b"one", 0) == 3 && send(seq_a, b"two!", 0) == 4,
        "seq sends"
    );
    check!(
        recv(seq_b, &mut buf, MSG_PEEK) == 3,
        "seqpacket peek is one message"
    );
    check!(
        recv(seq_b, &mut buf, 0) == 3 && recv(seq_b, &mut buf, 0) == 4,
        "seqpacket reads"
    );
    for fd in [a, b, seq_a, seq_b] {
        sys(3, &[fd]);
    }
    Ok(())
}

/// `sendmsg` gathers a stream's iovec in order; `recvmsg` reads it back and
/// reports no control data.
pub fn msg_sendmsg_recvmsg() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair()?;
    let (p1, p2) = (b"abc".as_slice(), b"defg".as_slice());
    let iov = [p1.as_ptr() as u64, 3, p2.as_ptr() as u64, 4];
    // struct msghdr: name, namelen(+pad), iov, iovlen, control, controllen, flags.
    let msg = [0u64, 0, iov.as_ptr() as u64, 2, 0, 0, 0];
    check!(sys(SENDMSG, &[a, msg.as_ptr() as u64, 0]) == 7, "sendmsg");
    let mut buf = [0u8; 16];
    let riov = [buf.as_mut_ptr() as u64, 16];
    let mut rmsg = [0u64, 0, riov.as_ptr() as u64, 1, 0, 99, 0xff];
    check!(
        sys(RECVMSG, &[b, rmsg.as_mut_ptr() as u64, 0]) == 7,
        "recvmsg"
    );
    check!(&buf[..7] == b"abcdefg", "recvmsg bytes {:?}", &buf[..7]);
    check!(
        rmsg[5] == 0 && rmsg[6] as u32 == 0,
        "control/flags not cleared"
    );
    let control = [0u8; 16];
    let cmsg = [
        0u64,
        0,
        iov.as_ptr() as u64,
        2,
        control.as_ptr() as u64,
        16,
        0,
    ];
    check!(
        sys(SENDMSG, &[a, cmsg.as_ptr() as u64, 0]) == EOPNOTSUPP,
        "control data accepted"
    );
    check!(
        sys_checked(SENDMSG, &[a, 8, 0]) == EFAULT,
        "bad msghdr accepted"
    );
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

/// `MSG_OOB` and unknown bits are refused rather than ignored.
pub fn msg_bad_flags() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair()?;
    let mut buf = [0u8; 4];
    check!(send(a, b"x", MSG_OOB) == EOPNOTSUPP, "send OOB");
    check!(recv(b, &mut buf, MSG_OOB) == EOPNOTSUPP, "recv OOB");
    check!(
        send(a, b"x", 0x10_0000) == EINVAL,
        "an unknown send flag was ignored"
    );
    check!(
        recv(b, &mut buf, 0x20_0000) == EINVAL,
        "an unknown recv flag was ignored"
    );
    let (r, _w) = pipe()?;
    check!(
        recv(r, &mut buf, 0) == neg(88),
        "recv on a pipe is not ENOTSOCK"
    );
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

/// Many rounds of mixed sends, peeks and reads keep the byte stream intact.
pub fn msg_soak() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair()?;
    let mut sent = 0u32;
    let mut read = 0u32;
    for round in 0..3000u32 {
        let chunk: Vec<u8> = (0..(round % 37 + 1)).map(|i| (sent + i) as u8).collect();
        check!(
            send(a, &chunk, MSG_DONTWAIT) == chunk.len() as u64,
            "round {round}: send"
        );
        sent += chunk.len() as u32;
        let mut buf = [0u8; 64];
        let peeked = recv(b, &mut buf, MSG_PEEK | MSG_DONTWAIT);
        check!(
            peeked as u32 == (sent - read).min(64),
            "round {round}: peek {peeked}"
        );
        check!(
            buf[0] == read as u8,
            "round {round}: peek starts at {}",
            buf[0]
        );
        let take = (round % 50 + 1) as usize;
        let n = recv(b, &mut buf[..take], MSG_DONTWAIT);
        check!((n as i64) > 0, "round {round}: read {n:#x}");
        for (i, byte) in buf[..n as usize].iter().enumerate() {
            check!(
                *byte == (read + i as u32) as u8,
                "round {round}: byte {i} out of order"
            );
        }
        read += n as u32;
    }
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

/// A seqpacket pair `(a, b)`.
fn seqpacket_pair() -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    check!(
        sys(53, &[1, 5, 0, sv.as_mut_ptr() as u64]) == 0,
        "seqpacket pair"
    );
    Ok((sv[0] as u64, sv[1] as u64))
}

/// `recvmsg` into `bufs` (one iovec segment each, empty ones included):
/// `(result, msg_flags)`.
fn recvmsg_into(fd: u64, bufs: &mut [&mut [u8]], flags: u64) -> (u64, u32) {
    let iov: Vec<u64> = bufs
        .iter_mut()
        .flat_map(|buf| [buf.as_mut_ptr() as u64, buf.len() as u64])
        .collect();
    let mut msg = [0u64, 0, iov.as_ptr() as u64, bufs.len() as u64, 0, 0, 0];
    let n = sys(RECVMSG, &[fd, msg.as_mut_ptr() as u64, flags]);
    (n, msg[6] as u32)
}

const MSG_TRUNC: u32 = 0x20;

/// `recvmsg` scatters across every segment in order: one whole message on a
/// seqpacket socket against the combined capacity (`MSG_TRUNC` only when
/// the message was really longer, and the next message is intact), and a
/// stream read across segments, also with `MSG_WAITALL`.
pub fn msg_recvmsg_scatters() -> Result<(), String> {
    fresh()?;
    let (a, b) = seqpacket_pair()?;
    let (mut x, mut empty, mut y, mut z) = ([0u8; 3], [0u8; 0], [0u8; 4], [0u8; 5]);
    check!(send(a, b"0123456789", 0) == 10, "send 10");
    let got = recvmsg_into(b, &mut [&mut x, &mut empty, &mut y, &mut z], 0);
    check!(got == (10, 0), "a message across segments: {got:?}");
    check!(
        &x == b"012" && &y == b"3456" && &z[..3] == b"789",
        "scattered {x:?} {y:?} {z:?}"
    );
    check!(send(a, b"abcdefgh", 0) == 8, "send 8");
    let (mut p, mut q) = ([0u8; 3], [0u8; 5]);
    let got = recvmsg_into(b, &mut [&mut p, &mut q], 0);
    check!(got == (8, 0), "an exact fit is not truncated: {got:?}");
    check!(&p == b"abc" && &q == b"defgh", "exact fit bytes");
    check!(
        send(a, b"ABCDEFGHIJ", 0) == 10 && send(a, b"z", 0) == 1,
        "send"
    );
    let (mut p, mut q) = ([0u8; 3], [0u8; 4]);
    let got = recvmsg_into(b, &mut [&mut p, &mut q], 0);
    check!(got == (7, MSG_TRUNC), "a longer message: {got:?}");
    check!(&p == b"ABC" && &q == b"DEFG", "truncated bytes");
    let mut one = [0u8; 4];
    check!(
        recv(b, &mut one, 0) == 1 && one[0] == b'z',
        "the next message after a truncation"
    );
    // A stream: one read spread over the segments, and WAITALL fills them.
    let (s, t) = socketpair()?;
    check!(send(s, b"hello world", 0) == 11, "stream send");
    let (mut p, mut q, mut r) = ([0u8; 4], [0u8; 4], [0u8; 8]);
    let got = recvmsg_into(t, &mut [&mut p, &mut q, &mut r], 0);
    check!(got == (11, 0), "stream recvmsg {got:?}");
    check!(
        &p == b"hell" && &q == b"o wo" && &r[..3] == b"rld",
        "stream bytes"
    );
    check!(send(s, b"abcdef", 0) == 6, "stream send 6");
    let (mut p, mut q) = ([0u8; 2], [0u8; 4]);
    let got = recvmsg_into(t, &mut [&mut p, &mut q], MSG_WAITALL);
    check!(
        got == (6, 0) && &p == b"ab" && &q == b"cdef",
        "WAITALL {got:?}"
    );
    for fd in [a, b, s, t] {
        sys(3, &[fd]);
    }
    Ok(())
}

/// Soak: thousands of seqpacket messages read through varying iovec splits
/// arrive whole and in order, truncated exactly when longer than the iovec.
pub fn msg_recvmsg_soak() -> Result<(), String> {
    fresh()?;
    let (a, b) = seqpacket_pair()?;
    for round in 0..2000usize {
        let len = round % 61 + 1;
        let message: Vec<u8> = (0..len).map(|i| (round + i) as u8).collect();
        check!(
            send(a, &message, MSG_DONTWAIT) == len as u64,
            "round {round}: send"
        );
        let mut first = alloc::vec![0u8; round % 7];
        let mut second = alloc::vec![0u8; round % 23];
        let mut third = alloc::vec![0u8; round % 41];
        let room = first.len() + second.len() + third.len();
        let got = recvmsg_into(b, &mut [&mut first, &mut second, &mut third], 0);
        let kept = len.min(room);
        let truncated = if len > room { MSG_TRUNC } else { 0 };
        check!(
            got == (kept as u64, truncated),
            "round {round}: {got:?}, want ({kept}, {truncated})"
        );
        let joined: Vec<u8> = first.iter().chain(&second).chain(&third).copied().collect();
        check!(
            joined[..kept] == message[..kept],
            "round {round}: bytes out of order"
        );
    }
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

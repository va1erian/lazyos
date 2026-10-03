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

/// `sendmsg` gathers a stream's iovec in order; `recvmsg` reads into the
/// first segment and reports no control data.
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

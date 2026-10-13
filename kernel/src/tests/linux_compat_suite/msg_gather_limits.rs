//! The `SOCK_SEQPACKET` gather's limits: the segment count, records up to the
//! pipe capacity, a full queue with `MSG_DONTWAIT`, and a soak of large
//! records (helpers in `msg_gather.rs`).

use super::msg_gather::*;
use super::*;
use crate::ipc::pipe;

const UIO_MAXIOV: usize = 1024;

/// Exactly `UIO_MAXIOV` (1024) segments make one record; one more is `EINVAL`
/// for `writev` and `EMSGSIZE` for `sendmsg` (what Linux answers).
pub fn msg_gather_segment_limit() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let data: Vec<u8> = (0..UIO_MAXIOV + 1).map(|i| i as u8).collect();
    let segments: Vec<&[u8]> = data.chunks(1).collect();
    let mut buf = alloc::vec![0u8; 2048];
    let sent = writev_gather(a, &segments[..UIO_MAXIOV]);
    check!(
        sent == UIO_MAXIOV as u64,
        "writev of 1024 segments: {sent:#x}"
    );
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == UIO_MAXIOV as u64
            && buf[..UIO_MAXIOV] == data[..UIO_MAXIOV],
        "its record"
    );
    let sent = sendmsg_gather(a, &segments[..UIO_MAXIOV], &[], 0);
    check!(
        sent == UIO_MAXIOV as u64,
        "sendmsg of 1024 segments: {sent:#x}"
    );
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == UIO_MAXIOV as u64,
        "its record"
    );
    let sent = writev_gather(a, &segments);
    check!(sent == EINVAL, "writev of 1025 segments: {sent:#x}");
    let sent = sendmsg_gather(a, &segments, &[], 0);
    check!(sent == EMSGSIZE, "sendmsg of 1025 segments: {sent:#x}");
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "a refused vector queued bytes"
    );
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

/// Records up to the pipe capacity: one just under and one exactly at it make
/// one record each and read back intact; one byte more is `EMSGSIZE`.
pub fn msg_gather_near_capacity() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let data: Vec<u8> = (0..CAPACITY + 1)
        .map(|i| (i * 31 + i / 251) as u8)
        .collect();
    let mut buf = alloc::vec![0u8; CAPACITY + 16];
    for size in [CAPACITY - 1, CAPACITY] {
        let (head, tail) = data[..size].split_at(size / 3);
        let sent = writev_gather(a, &[head, tail]);
        check!(sent == size as u64, "writev of {size}: {sent:#x}");
        let got = recv(b, &mut buf, 0);
        check!(
            got == size as u64 && buf[..size] == data[..size],
            "record of {size}: {got:#x}"
        );
        let sent = sendmsg_gather(a, &[head, tail], &[], 0);
        check!(sent == size as u64, "sendmsg of {size}: {sent:#x}");
        let got = recv(b, &mut buf, 0);
        check!(
            got == size as u64 && buf[..size] == data[..size],
            "record of {size}: {got:#x}"
        );
    }
    let (head, tail) = data.split_at(CAPACITY / 2);
    check!(
        writev_gather(a, &[head, tail]) == EMSGSIZE,
        "capacity + 1 was accepted"
    );
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "an oversized gather queued bytes"
    );
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

/// `MSG_DONTWAIT` on a gather against a full queue is `EAGAIN`, with or
/// without descriptors (which stay with the sender), and the gather goes
/// through once the reader has made room.
pub fn msg_gather_dontwait_full_queue() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (r, w) = pipe_nonblocking()?;
    let chunk = alloc::vec![0x5Au8; 4096];
    let mut filled = 0usize;
    while send_one(a, &chunk) == chunk.len() as u64 {
        filled += 1;
        check!(filled <= 64, "the queue never filled");
    }
    check!(filled > 0, "not one record fitted");
    let (p, q) = (alloc::vec![1u8; 3000], alloc::vec![2u8; 3000]);
    let got = sendmsg_gather(a, &[&p, &q], &[], MSG_DONTWAIT);
    check!(got == EAGAIN, "a gather against a full queue: {got:#x}");
    let got = sendmsg_gather(a, &[&p, &q], &rights_control(w), MSG_DONTWAIT);
    check!(
        got == EAGAIN,
        "a gather with rights against a full queue: {got:#x}"
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    let mut buf = alloc::vec![0u8; 8192];
    for _ in 0..filled {
        check!(
            recv(b, &mut buf, MSG_DONTWAIT) == chunk.len() as u64,
            "draining"
        );
    }
    let got = sendmsg_gather(a, &[&p, &q], &rights_control(w), MSG_DONTWAIT);
    check!(got == 6000, "the gather once there is room: {got:#x}");
    let got = recv_record(b, &mut buf, 0);
    check!(
        got.n == 6000 && got.fd.is_some(),
        "its record {:#x} {:?}",
        got.n,
        got.fd
    );
    for fd in [got.fd.unwrap_or(0), w, r, a, b] {
        sys(3, &[fd]);
    }
    Ok(())
}

fn send_one(fd: u64, data: &[u8]) -> u64 {
    sys(
        44,
        &[
            fd,
            data.as_ptr() as u64,
            data.len() as u64,
            MSG_DONTWAIT,
            0,
            0,
        ],
    )
}

/// Soak: hundreds of records of up to tens of KB, gathered from three
/// segments through `sendmsg` and `writev` alternately, some carrying a
/// descriptor, arrive whole; the fd table, the pipes, the descriptors in
/// flight and the frames are where they started.
pub fn msg_gather_large_soak() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (r, w) = pipe_nonblocking()?;
    let source: Vec<u8> = (0..CAPACITY).map(|i| (i * 13 + i / 7) as u8).collect();
    let mut out = alloc::vec![0u8; CAPACITY + 16];
    let mut round = |index: usize, len: usize| -> Result<(), String> {
        let (l1, l2) = (len / 3, len / 5);
        let start = index * 977 % (CAPACITY - len);
        let message = &source[start..start + len];
        let (s1, rest) = message.split_at(l1);
        let (s2, s3) = rest.split_at(l2);
        let with_rights = index % 5 == 0;
        let sent = if with_rights || index % 2 == 0 {
            let control = if with_rights {
                rights_control(w).to_vec()
            } else {
                Vec::new()
            };
            sendmsg_gather(a, &[s1, s2, s3], &control, 0)
        } else {
            writev_gather(a, &[s1, s2, s3])
        };
        check!(sent == len as u64, "round {index}: sent {sent:#x} of {len}");
        let got = recv_record(b, &mut out, 0);
        check!(
            got.n == len as u64,
            "round {index}: record of {:#x}, want {len}",
            got.n
        );
        check!(
            out[..len] == *message,
            "round {index}: bytes of a {len}-byte record"
        );
        check!(
            got.fd.is_some() == with_rights,
            "round {index}: descriptor {:?}",
            got.fd
        );
        if let Some(fd) = got.fd {
            sys(3, &[fd]);
        }
        Ok(())
    };
    // The largest records and a descriptor first, so the heap and tables have grown.
    round(0, CAPACITY - 1)?;
    round(1, CAPACITY / 2)?;
    let (fds, pipes) = (open_fds(), pipe::Pipe::live());
    let frames = crate::mem::frame_stats().live();
    for index in 2..700usize {
        round(index, 1 + index * 7919 % 49_000)?;
    }
    check!(
        open_fds() == fds,
        "descriptors leaked: {} -> {}",
        fds,
        open_fds()
    );
    check!(
        pipe::Pipe::live() == pipes,
        "pipes {} -> {}",
        pipes,
        pipe::Pipe::live()
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    let after = crate::mem::frame_stats().live();
    check!(after == frames, "frames {frames} -> {after}");
    sys(3, &[w]);
    let mut byte = [0u8; 1];
    check!(
        sys(0, &[r, byte.as_mut_ptr() as u64, 1]) == 0,
        "a received write end was never closed"
    );
    for fd in [r, a, b] {
        sys(3, &[fd]);
    }
    Ok(())
}

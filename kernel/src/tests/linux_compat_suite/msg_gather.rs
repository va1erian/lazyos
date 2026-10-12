//! The `SOCK_SEQPACKET` gather (`io::send_gathered`, reached by `writev` and
//! `sendmsg`): descriptors riding a gathered record and what a refused gather
//! leaves behind. The limits, a full queue and the soak are in
//! `msg_gather_limits.rs`.

use super::*;
use crate::ipc::pipe;

pub(super) const SENDMSG: u64 = 46;
pub(super) const RECVMSG: u64 = 47;
pub(super) const WRITEV: u64 = 20;
pub(super) const MSG_DONTWAIT: u64 = 0x40;
pub(super) const F_SETFL: u64 = 4;
pub(super) const O_NONBLOCK: u64 = 0o4000;
pub(super) const CAPACITY: usize = pipe::CAPACITY;

/// A seqpacket pair.
pub(super) fn pair() -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    check!(
        sys(53, &[1, 5, 0, sv.as_mut_ptr() as u64]) == 0,
        "seqpacket pair"
    );
    Ok((sv[0] as u64, sv[1] as u64))
}

/// A pipe whose read end never blocks, so a leaked write end is `EAGAIN`
/// instead of a hung test.
pub(super) fn pipe_nonblocking() -> Result<(u64, u64), String> {
    let (r, w) = pipe()?;
    check!(sys(72, &[r, F_SETFL, O_NONBLOCK]) == 0, "O_NONBLOCK");
    Ok((r, w))
}

/// A `SCM_RIGHTS` control message for one descriptor, padded to `CMSG_SPACE`.
pub(super) fn rights_control(fd: u64) -> [u8; 24] {
    let mut control = [0u8; 24];
    control[..8].copy_from_slice(&20u64.to_le_bytes());
    control[8..12].copy_from_slice(&1i32.to_le_bytes()); // SOL_SOCKET
    control[12..16].copy_from_slice(&1i32.to_le_bytes()); // SCM_RIGHTS
    control[16..20].copy_from_slice(&(fd as i32).to_le_bytes());
    control
}

/// `sendmsg` of `parts` (kernel buffers: the harness trusts them) with an
/// optional control buffer.
pub(super) fn sendmsg_gather(fd: u64, parts: &[&[u8]], control: &[u8], flags: u64) -> u64 {
    let iov: Vec<u64> = parts
        .iter()
        .flat_map(|part| [part.as_ptr() as u64, part.len() as u64])
        .collect();
    let (ptr, len) = if control.is_empty() {
        (0, 0)
    } else {
        (control.as_ptr() as u64, control.len() as u64)
    };
    let msg = [
        0u64,
        0,
        iov.as_ptr() as u64,
        parts.len() as u64,
        ptr,
        len,
        0,
    ];
    sys(SENDMSG, &[fd, msg.as_ptr() as u64, flags])
}

/// `writev` of `parts`.
pub(super) fn writev_gather(fd: u64, parts: &[&[u8]]) -> u64 {
    let iov: Vec<u64> = parts
        .iter()
        .flat_map(|part| [part.as_ptr() as u64, part.len() as u64])
        .collect();
    sys(WRITEV, &[fd, iov.as_ptr() as u64, parts.len() as u64])
}

/// What one `recvmsg` of a record returned: the result, the descriptor it
/// carried (if any) and `msg_flags`.
pub(super) struct Record {
    pub(super) n: u64,
    pub(super) fd: Option<u64>,
    pub(super) flags: u32,
}

/// `recvmsg` into `buf` split over two segments, with room for one descriptor.
pub(super) fn recv_record(fd: u64, buf: &mut [u8], flags: u64) -> Record {
    let (first, second) = buf.split_at_mut(buf.len() / 2);
    let iov = [
        first.as_mut_ptr() as u64,
        first.len() as u64,
        second.as_mut_ptr() as u64,
        second.len() as u64,
    ];
    let mut control = [0xAAu8; 20];
    let mut msg = [
        0u64,
        0,
        iov.as_ptr() as u64,
        2,
        control.as_mut_ptr() as u64,
        control.len() as u64,
        0,
    ];
    let n = sys(RECVMSG, &[fd, msg.as_mut_ptr() as u64, flags]);
    let carried = (n as i64) >= 0 && msg[5] as usize >= 20;
    Record {
        n,
        fd: carried.then(|| i32::from_le_bytes(control[16..20].try_into().unwrap()) as u64),
        flags: msg[6] as u32,
    }
}

/// `recv` of one record with no control buffer.
pub(super) fn recv(fd: u64, buf: &mut [u8], flags: u64) -> u64 {
    sys(
        45,
        &[fd, buf.as_mut_ptr() as u64, buf.len() as u64, flags, 0, 0],
    )
}

pub(super) fn open_fds() -> usize {
    task::harness::fd_open_count(task::KERNEL_TASK)
}

/// A bump arena in mmap'd user memory: unlike kernel buffers it passes the
/// validation `sys_checked` turns on, so a bad pointer among good ones faults
/// at exactly the segment that is bad.
pub(super) struct Arena {
    base: u64,
    len: u64,
    used: u64,
}

impl Arena {
    fn new(len: u64) -> Result<Arena, String> {
        Ok(Arena {
            base: mmap_prot(len, RW)?,
            len,
            used: 0,
        })
    }

    fn put(&mut self, bytes: &[u8]) -> u64 {
        let at = self.base + self.used;
        assert!(self.used + bytes.len() as u64 <= self.len, "arena full");
        // SAFETY: `at..at + len` lies inside the mapping `new` made, read-write.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len()) };
        self.used = (self.used + bytes.len() as u64 + 7) & !7;
        at
    }

    fn put_words(&mut self, words: &[u64]) -> u64 {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        self.put(&bytes)
    }

    /// `sendmsg` with validation on: segments `(ptr, len)` and a control buffer.
    fn sendmsg(&mut self, fd: u64, parts: &[(u64, u64)], control: &[u8]) -> u64 {
        let words: Vec<u64> = parts.iter().flat_map(|&(p, l)| [p, l]).collect();
        let iov = self.put_words(&words);
        let control_ptr = if control.is_empty() {
            0
        } else {
            self.put(control)
        };
        let msg = self.put_words(&[
            0,
            0,
            iov,
            parts.len() as u64,
            control_ptr,
            control.len() as u64,
            0,
        ]);
        sys_checked(SENDMSG, &[fd, msg, 0])
    }

    fn writev(&mut self, fd: u64, parts: &[(u64, u64)]) -> u64 {
        let words: Vec<u64> = parts.iter().flat_map(|&(p, l)| [p, l]).collect();
        let iov = self.put_words(&words);
        sys_checked(WRITEV, &[fd, iov, parts.len() as u64])
    }

    fn release(self) {
        unmap(self.base, self.len);
    }
}

/// `sendmsg` with `SCM_RIGHTS` and two segments is one record: `recvmsg`
/// gets both segments' bytes and the descriptor together, and no second
/// record follows.
pub fn msg_gather_carries_rights() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (r, w) = pipe_nonblocking()?;
    let sent = sendmsg_gather(a, &[b"head", b"body!"], &rights_control(w), 0);
    check!(sent == 9, "sendmsg gather with rights: {sent:#x}");
    let mut buf = [0u8; 16];
    let got = recv_record(b, &mut buf, 0);
    check!(
        got.n == 9 && got.flags == 0,
        "recvmsg {:#x} flags {:#x}",
        got.n,
        got.flags
    );
    check!(&buf[..9] == b"headbody!", "record bytes {:?}", &buf[..9]);
    let Some(received) = got.fd.filter(|fd| *fd != w) else {
        return Err(format!("no new descriptor arrived: {:?}", got.fd));
    };
    check!(
        sys(1, &[received, b"z".as_ptr() as u64, 1]) == 1,
        "write via the received end"
    );
    let mut byte = [0u8; 1];
    check!(
        sys(0, &[r, byte.as_mut_ptr() as u64, 1]) == 1 && byte[0] == b'z',
        "the received descriptor is the pipe's write end"
    );
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "a second record appeared"
    );
    for fd in [received, w] {
        sys(3, &[fd]);
    }
    check!(
        sys(0, &[r, byte.as_mut_ptr() as u64, 1]) == 0,
        "a write end leaked (no EOF)"
    );
    check!(
        pipe::in_flight() == 0,
        "{} descriptors in flight",
        pipe::in_flight()
    );
    for fd in [a, b, r] {
        sys(3, &[fd]);
    }
    Ok(())
}

/// A gather refused with `EMSGSIZE` or `EFAULT` queues nothing and leaves the
/// descriptors it would have carried alone: the sender's end is still open,
/// no duplicate is in flight or leaked, and the same message then goes out.
pub fn msg_gather_refusals_keep_rights() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (r, w) = pipe_nonblocking()?;
    let fds_before = open_fds();
    let mut arena = Arena::new(8 * 4096)?;
    let abc = arena.put(b"abc");
    let control = rights_control(w);
    let oversized = [(abc, CAPACITY as u64), (abc, 3)];
    let faulting = [(abc, 3), (8u64, 4)];
    for (what, parts, want) in [
        ("EMSGSIZE", &oversized, EMSGSIZE),
        ("EFAULT", &faulting, EFAULT),
    ] {
        let got = arena.sendmsg(a, parts, &control);
        check!(got == want, "{what} gather with rights: {got:#x}");
        let mut buf = [0u8; 16];
        check!(
            recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
            "{what}: bytes were queued"
        );
        check!(
            pipe::in_flight() == 0,
            "{what}: {} in flight",
            pipe::in_flight()
        );
        check!(open_fds() == fds_before, "{what}: the table changed");
    }
    // The sender's end is open and nothing else holds a duplicate of it.
    let z = arena.put(b"z");
    check!(
        sys_checked(1, &[w, z, 1]) == 1,
        "the sender's end was closed"
    );
    sys(3, &[w]);
    let out = arena.put(&[0u8; 8]);
    check!(
        sys_checked(0, &[r, out, 1]) == 1,
        "the byte written before the close"
    );
    check!(
        sys_checked(0, &[r, out, 1]) == 0,
        "a refused gather leaked a duplicate (no EOF)"
    );
    // The same message, now valid, still carries a descriptor.
    let (r2, w2) = pipe_nonblocking()?;
    let sent = arena.sendmsg(a, &[(abc, 3), (abc, 3)], &rights_control(w2));
    check!(sent == 6, "a valid gather afterwards: {sent:#x}");
    let mut buf = [0u8; 16];
    let got = recv_record(b, &mut buf, 0);
    check!(
        got.n == 6 && got.fd.is_some(),
        "the descriptor arrived: {:?}",
        got.fd
    );
    for fd in [got.fd.unwrap_or(0), w2, r2, r, a, b] {
        sys(3, &[fd]);
    }
    arena.release();
    check!(
        pipe::in_flight() == 0,
        "{} in flight at the end",
        pipe::in_flight()
    );
    Ok(())
}

/// `writev` on a seqpacket pair: `EFAULT` and `EMSGSIZE` queue nothing; an
/// all-empty vector is `0` and no record (as `write` of nothing is).
pub fn msg_writev_gather_errors() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let mut arena = Arena::new(4 * 4096)?;
    let abc = arena.put(b"abc");
    let mut buf = [0u8; 16];
    let got = arena.writev(a, &[(abc, 3), (8, 4)]);
    check!(got == EFAULT, "writev with a bad segment: {got:#x}");
    let got = arena.writev(a, &[(abc, CAPACITY as u64), (abc, 3)]);
    check!(got == EMSGSIZE, "writev over the capacity: {got:#x}");
    check!(
        recv(b, &mut buf, MSG_DONTWAIT) == EAGAIN,
        "a refused writev queued bytes"
    );
    let got = arena.writev(a, &[(abc, 0), (abc, 0), (abc, 0)]);
    check!(got == 0, "an all-empty writev: {got:#x}");
    let got = sendmsg_gather(a, &[b"", b""], &[], 0);
    check!(got == 0, "an all-empty sendmsg: {got:#x}");
    let got = recv(b, &mut buf, MSG_DONTWAIT);
    check!(
        got == EAGAIN,
        "an all-empty gather queued a record: {got:#x}"
    );
    // A good one still works afterwards.
    check!(
        arena.writev(a, &[(abc, 3), (abc, 2)]) == 5,
        "a good writev afterwards"
    );
    check!(recv(b, &mut buf, MSG_DONTWAIT) == 5, "its record");
    arena.release();
    sys(3, &[a]);
    sys(3, &[b]);
    Ok(())
}

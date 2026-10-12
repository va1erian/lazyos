//! `SCM_RIGHTS` delivery: a round trip, close-on-exec, truncation, the
//! refusals and `MSG_PEEK`.

use super::*;

/// A pipe's write end travels to another process, which writes through it;
/// the sender closing its own copy right after `sendmsg` changes nothing.
pub fn scmrights_round_trip() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"x", &[p.w]) == 1, "sendmsg");
    check!(pipe::in_flight() == 1, "in flight: {}", pipe::in_flight());
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.r, p.a] {
        close(fd);
    }
    let before = open_count(p.child);
    let mut buf = [0u8; 8];
    let got = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && buf[0] == b'x',
        "recvmsg returned {:#x}",
        got.n
    );
    check!(got.fds.len() == 1, "{} descriptors arrived", got.fds.len());
    check!(got.flags & MSG_CTRUNC == 0, "truncated: {:#x}", got.flags);
    let received = got.fds[0];
    check!(open_count(p.child) == before + 1, "not installed");
    check!(
        task::harness::fd_kind_at(p.child, received as usize) == task::FdKind::Pipe,
        "not a pipe"
    );
    check!(
        !task::harness::fd_cloexec_at(p.child, received as usize),
        "close-on-exec without the flag"
    );
    check!(
        sys(1, &[received, b"ping".as_ptr() as u64, 4]) == 4,
        "write through the received descriptor"
    );
    close(received);
    check!(pipe::in_flight() == 0, "still in flight after delivery");
    task::harness::switch_current(p.leader);
    let mut data = [0u8; 8];
    check!(
        sys(0, &[p.r, data.as_mut_ptr() as u64, 8]) == 4 && &data[..4] == b"ping",
        "the data did not reach the pipe"
    );
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

/// `MSG_CMSG_CLOEXEC` marks the received descriptors close-on-exec.
pub fn scmrights_cloexec() -> Result<(), String> {
    let p = processes()?;
    check!(send_fds(p.a, b"c", &[p.w, p.r]) == 1, "sendmsg");
    task::harness::switch_current(p.child);
    let mut buf = [0u8; 4];
    let got = recv_fds(p.b, &mut buf, Some(2), MSG_CMSG_CLOEXEC);
    check!(got.fds.len() == 2, "{} descriptors", got.fds.len());
    for fd in &got.fds {
        check!(
            task::harness::fd_cloexec_at(p.child, *fd as usize),
            "descriptor {fd} is not close-on-exec"
        );
    }
    check!(
        task::harness::fd_kind_at(p.child, got.fds[0] as usize) == task::FdKind::Pipe
            && task::harness::fd_kind_at(p.child, got.fds[1] as usize) == task::FdKind::Pipe,
        "both should be pipe ends"
    );
    task::harness::reset();
    Ok(())
}

/// A control buffer too small for every descriptor installs what fits,
/// closes the rest and says `MSG_CTRUNC`; no control buffer at all closes
/// them all.
pub fn scmrights_ctrunc() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"1", &[p.w, p.w, p.w]) == 1, "send three");
    check!(send_fds(p.a, b"2", &[p.w]) == 1, "send one more");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    let before = open_count(p.child);
    let mut buf = [0u8; 4];
    let got = recv_fds(p.b, &mut buf, Some(1), 0);
    check!(got.n == 1 && got.fds.len() == 1, "{} fds", got.fds.len());
    check!(
        got.flags & MSG_CTRUNC != 0,
        "no MSG_CTRUNC: {:#x}",
        got.flags
    );
    check!(open_count(p.child) == before + 1, "more than one installed");
    let none = recv_fds(p.b, &mut buf, None, 0);
    check!(none.n == 1 && buf[0] == b'2', "second message");
    check!(none.fds.is_empty(), "descriptors without a control buffer");
    check!(
        none.flags & MSG_CTRUNC != 0,
        "no MSG_CTRUNC without a buffer"
    );
    check!(open_count(p.child) == before + 1, "a descriptor leaked");
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    // The three closed copies and the fourth are really gone: the one
    // installed descriptor is the pipe's last write end.
    task::harness::switch_current(p.leader);
    check!(!at_eof(p.r), "the installed copy should hold the pipe open");
    task::harness::switch_current(p.child);
    close(got.fds[0]);
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a truncated descriptor was not closed");
    task::harness::reset();
    settled(pipes_before)
}

/// Bad descriptors, too many of them, credentials, and malformed control
/// data are refused before anything is sent.
pub fn scmrights_refusals() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"x", &[p.w, 99]) == EBADF, "a closed fd");
    let many = alloc::vec![p.w as i32; 254];
    check!(
        sendmsg_control(p.a, b"x", &rights_control(&many), 0) == EINVAL,
        "254 descriptors"
    );
    let cred = control_message(SOL_SOCKET, SCM_CREDENTIALS, &[0u8; 12]);
    check!(
        sendmsg_control(p.a, b"x", &cred, 0) == EOPNOTSUPP,
        "SCM_CREDENTIALS"
    );
    let other = control_message(SOL_SOCKET, 7, &[0u8; 4]);
    check!(
        sendmsg_control(p.a, b"x", &other, 0) == EINVAL,
        "unknown type"
    );
    let level = control_message(6, SCM_RIGHTS, &[0u8; 4]);
    check!(
        sendmsg_control(p.a, b"x", &level, 0) == EINVAL,
        "unknown level"
    );
    check!(
        sendmsg_control(p.a, b"x", &[0u8; 16], 0) == EINVAL,
        "a zero-length control message"
    );
    let mut short = rights_control(&[p.w as i32]);
    short[0] = 64; // cmsg_len past the buffer
    check!(
        sendmsg_control(p.a, b"x", &short, 0) == EINVAL,
        "cmsg_len past the end"
    );
    let mut huge = rights_control(&[p.w as i32]);
    huge[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    check!(
        sendmsg_control(p.a, b"x", &huge, 0) == EINVAL,
        "a cmsg_len of u64::MAX"
    );
    check!(
        send_fds(p.r, b"x", &[p.w]) == ENOTSOCK,
        "descriptors on a pipe"
    );
    check!(
        pipe::in_flight() == 0,
        "refusals left descriptors in flight"
    );
    // 253 is the limit and works.
    let most = alloc::vec![p.w as i32; 253];
    check!(
        sendmsg_control(p.a, b"y", &rights_control(&most), 0) == 1,
        "253 descriptors"
    );
    check!(pipe::in_flight() == 253, "in flight: {}", pipe::in_flight());
    task::harness::switch_current(p.child);
    let mut buf = [0u8; 4];
    let got = recv_fds(p.b, &mut buf, Some(253), 0);
    check!(
        got.fds.len() == 253 && got.flags & MSG_CTRUNC == 0,
        "{} of 253 arrived, flags {:#x}",
        got.fds.len(),
        got.flags
    );
    for fd in got.fds {
        close(fd);
    }
    task::harness::reset();
    settled(pipes_before)
}

/// `MSG_PEEK` delivers duplicates and leaves the message queued.
pub fn scmrights_peek() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"p", &[p.w]) == 1, "sendmsg");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    let mut buf = [0u8; 4];
    let peeked = recv_fds(p.b, &mut buf, Some(2), MSG_PEEK);
    check!(
        peeked.n == 1 && peeked.fds.len() == 1,
        "peek got {} fds",
        peeked.fds.len()
    );
    check!(pipe::in_flight() == 1, "the peek consumed the descriptors");
    let taken = recv_fds(p.b, &mut buf, Some(2), 0);
    check!(
        taken.n == 1 && taken.fds.len() == 1,
        "recv got {} fds",
        taken.fds.len()
    );
    check!(
        pipe::in_flight() == 0,
        "still in flight after the real read"
    );
    check!(peeked.fds[0] != taken.fds[0], "the same descriptor twice");
    close(peeked.fds[0]);
    task::harness::switch_current(p.leader);
    check!(!at_eof(p.r), "the received copy should still be open");
    task::harness::switch_current(p.child);
    close(taken.fds[0]);
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

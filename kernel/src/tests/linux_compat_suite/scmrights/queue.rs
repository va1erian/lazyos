//! `SCM_RIGHTS` while queued: dropped and orphaned messages, stream and
//! seqpacket boundaries, the in-flight limits and a soak.

use super::*;

/// A message nobody received is dropped with its socket: closing the
/// receiving end closes the descriptors queued for it.
pub fn scmrights_dropped_message_closes() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"d", &[p.w]) == 1, "sendmsg");
    check!(send_fds(p.a, b"e", &[p.w, p.w]) == 1, "sendmsg two");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    check!(
        !at_eof(p.r),
        "the queued write ends should keep the pipe open"
    );
    check!(pipe::in_flight() == 3, "in flight: {}", pipe::in_flight());
    // Both processes hold the receiving end: the last close purges.
    close(p.b);
    check!(pipe::in_flight() == 3, "purged while a reader remains");
    task::harness::switch_current(p.child);
    close(p.b);
    task::harness::switch_current(p.leader);
    check!(
        pipe::in_flight() == 0,
        "{} still in flight",
        pipe::in_flight()
    );
    check!(at_eof(p.r), "the dropped descriptors were not closed");
    task::harness::reset();
    settled(pipes_before)
}

/// A receiver that exits with messages queued leaves nothing behind.
pub fn scmrights_receiver_exit_releases() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    close(p.b); // the child's copy is the only receiving end now
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    check!(send_fds(p.a, b"q", &[p.w]) == 1, "sendmsg");
    close(p.w);
    check!(
        !at_eof(p.r),
        "the queued write end should hold the pipe open"
    );
    task::harness::finish(p.child, 0);
    check!(
        pipe::in_flight() == 0,
        "{} in flight after exit",
        pipe::in_flight()
    );
    check!(at_eof(p.r), "the exited receiver's queue was not purged");
    task::harness::reset();
    settled(pipes_before)
}

/// A stream read stops at the end of a message that carried descriptors,
/// and a plain `read` consumes (and closes) them.
pub fn scmrights_stream_boundaries() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"AA", &[p.w]) == 2, "first");
    check!(send_fds(p.a, b"BB", &[]) == 2, "second");
    check!(send_fds(p.a, b"CC", &[p.w]) == 2, "third");
    check!(send_fds(p.a, b"DD", &[p.w]) == 2, "fourth");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    let mut buf = [0u8; 16];
    let first = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        first.n == 2 && &buf[..2] == b"AA" && first.fds.len() == 1,
        "first read: {} bytes, {} fds",
        first.n,
        first.fds.len()
    );
    let plain = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        plain.n == 2 && &buf[..2] == b"BB" && plain.fds.is_empty(),
        "plain read: {} bytes, {} fds",
        plain.n,
        plain.fds.len()
    );
    let second = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        second.n == 2 && &buf[..2] == b"CC" && second.fds.len() == 1,
        "second read: {} bytes, {} fds",
        second.n,
        second.fds.len()
    );
    // `read(2)` has nowhere to put the descriptors: consumed and closed.
    check!(
        sys(0, &[p.b, buf.as_mut_ptr() as u64, 16]) == 2,
        "plain read of the fourth message"
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    close(first.fds[0]);
    close(second.fds[0]);
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

/// On a seqpacket socket each message keeps its own descriptors, including a
/// message longer than the buffer.
pub fn scmrights_seqpacket() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.sa, b"one", &[p.w]) == 3, "one");
    check!(send_fds(p.sa, b"two", &[]) == 3, "two");
    check!(send_fds(p.sa, b"three!", &[p.w, p.r]) == 6, "three");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.sa, p.r] {
        close(fd);
    }
    let mut buf = [0u8; 16];
    let one = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(
        one.n == 3 && one.fds.len() == 1,
        "one: {} fds",
        one.fds.len()
    );
    let two = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(two.n == 3 && two.fds.is_empty(), "two carried descriptors");
    let mut small = [0u8; 3];
    let three = recv_fds(p.sb, &mut small, Some(4), 0);
    check!(
        three.n == 3 && three.flags & MSG_TRUNC != 0 && three.fds.len() == 2,
        "three: {} bytes, {} fds, flags {:#x}",
        three.n,
        three.fds.len(),
        three.flags
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    for fd in one.fds.into_iter().chain(three.fds) {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

/// Plain bytes queued ahead of a message with descriptors are read apart
/// from it, by `recvmsg` and by a peek: a read never mixes a message that
/// has none with one that has.
pub fn scmrights_read_stops_before_rights() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"aaa", &[]) == 3, "plain message");
    check!(
        send_fds(p.a, b"bbb", &[p.w]) == 3,
        "message with a descriptor"
    );
    check!(send_fds(p.a, b"ccc", &[]) == 3, "plain message after it");
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.a, p.r] {
        close(fd);
    }
    let mut buf = [0u8; 16];
    let peeked = recv_fds(p.b, &mut buf, Some(4), MSG_PEEK);
    check!(
        peeked.n == 3 && &buf[..3] == b"aaa" && peeked.fds.is_empty(),
        "peek: {} bytes, {} fds",
        peeked.n,
        peeked.fds.len()
    );
    let first = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        first.n == 3 && &buf[..3] == b"aaa" && first.fds.is_empty(),
        "first read: {} bytes, {} fds",
        first.n,
        first.fds.len()
    );
    check!(pipe::in_flight() == 1, "the descriptor was consumed early");
    let peeked = recv_fds(p.b, &mut buf, Some(4), MSG_PEEK);
    check!(
        peeked.n == 3 && &buf[..3] == b"bbb" && peeked.fds.len() == 1,
        "second peek: {} bytes, {} fds",
        peeked.n,
        peeked.fds.len()
    );
    let second = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        second.n == 3 && &buf[..3] == b"bbb" && second.fds.len() == 1,
        "second read: {} bytes, {} fds",
        second.n,
        second.fds.len()
    );
    // The bytes after it come in a read of their own too.
    let third = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        third.n == 3 && &buf[..3] == b"ccc" && third.fds.is_empty(),
        "third read"
    );
    for fd in peeked.fds.into_iter().chain(second.fds) {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

/// On a seqpacket socket a message with descriptors and no bytes is a
/// datagram of its own, in order with the others; on a stream it carries
/// nothing and its descriptors are closed.
pub fn scmrights_empty_message_with_rights() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(
        send_fds(p.sa, b"", &[p.w]) == 0,
        "empty datagram with a descriptor"
    );
    // An empty message with no descriptors sends nothing at all.
    check!(send_fds(p.sa, b"", &[]) == 0, "nothing at all");
    check!(send_fds(p.sa, b"mid", &[]) == 3, "plain datagram");
    check!(
        send_fds(p.sa, b"", &[p.w, p.w]) == 0,
        "second empty datagram"
    );
    check!(send_fds(p.sa, b"", &[p.w]) == 0, "third empty datagram");
    check!(pipe::in_flight() == 4, "in flight: {}", pipe::in_flight());
    // A stream drops the descriptors of an empty message.
    check!(send_fds(p.a, b"", &[p.w]) == 0, "empty stream message");
    check!(
        pipe::in_flight() == 4,
        "a stream kept an empty message's descriptors"
    );
    close(p.w);
    task::harness::switch_current(p.child);
    for fd in [p.w, p.sa, p.r] {
        close(fd);
    }
    let mut buf = [0u8; 8];
    let peeked = recv_fds(p.sb, &mut buf, Some(4), MSG_PEEK);
    check!(
        peeked.n == 0 && peeked.fds.len() == 1,
        "peek: {} fds",
        peeked.fds.len()
    );
    let first = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(
        first.n == 0 && first.fds.len() == 1,
        "first: {} fds",
        first.fds.len()
    );
    let third = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(
        third.n == 3 && &buf[..3] == b"mid" && third.fds.is_empty(),
        "third"
    );
    let fourth = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(
        fourth.n == 0 && fourth.fds.len() == 2,
        "fourth: {} fds",
        fourth.fds.len()
    );
    let fifth = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(
        fifth.n == 0 && fifth.fds.len() == 1,
        "fifth: {} fds",
        fifth.fds.len()
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    for fd in peeked
        .fds
        .into_iter()
        .chain(first.fds)
        .chain(fourth.fds)
        .chain(fifth.fds)
    {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    check!(at_eof(p.r), "a write end leaked");
    task::harness::reset();
    settled(pipes_before)
}

/// The in-flight ceiling (four full descriptor tables) and the sender's
/// `Fds` quota refuse with `ETOOMANYREFS`; closing the receiver gives it all
/// back.
pub fn scmrights_in_flight_limits() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let cap = crate::limits::fd_max() * 4;
    let most = rights_control(&alloc::vec![p.w as i32; 253]);
    // Enough full messages to pass the ceiling, whatever `fd_max` is.
    let plenty = cap / 253 + 2;
    let mut sent = 0;
    loop {
        let result = sendmsg_control(p.a, b"m", &most, 0);
        if result == ETOOMANYREFS {
            break;
        }
        check!(result == 1, "send {sent}: {result:#x}");
        sent += 1;
        check!(sent < plenty, "no limit after {sent} messages (cap {cap})");
    }
    check!(
        pipe::in_flight() == sent * 253 && pipe::in_flight() + 253 > cap,
        "in flight {} at the limit {cap}",
        pipe::in_flight()
    );
    close(p.b);
    task::harness::switch_current(p.child);
    close(p.b);
    task::harness::switch_current(p.leader);
    check!(pipe::in_flight() == 0, "purge left {}", pipe::in_flight());
    // The sender's own quota: root is uncapped, so give uid 0 a small one.
    let held = quota::usage(0, Resource::Fds);
    quota::set_limit(0, Resource::Fds, held + 10);
    let (c, d) = socketpair()?;
    let refused = send_fds(c, b"q", &[p.w; 20]);
    let allowed = send_fds(c, b"q", &[p.w; 4]);
    quota::set_limit(0, Resource::Fds, u64::MAX);
    check!(refused == ETOOMANYREFS, "over quota: {refused:#x}");
    check!(allowed == 1, "within quota: {allowed:#x}");
    close(c);
    close(d);
    task::harness::reset();
    settled(pipes_before)
}

/// Thousands of descriptors passed and closed: nothing is left in any table,
/// no pipe or frame leaks, and nothing stays in flight.
pub fn scmrights_soak() -> Result<(), String> {
    let p = processes()?;
    // One warm round settles lazily grown tables and heap before measuring.
    let round_trip = |kind: usize| -> Result<(), String> {
        let (data_sock, recv_sock) = if kind % 2 == 0 {
            (p.a, p.b)
        } else {
            (p.sa, p.sb)
        };
        task::harness::switch_current(p.leader);
        let (r, w) = pipe_nonblocking()?;
        check!(
            send_fds(data_sock, b"s", &[r, w]) == 1,
            "kind {kind}: sendmsg"
        );
        close(r);
        close(w);
        task::harness::switch_current(p.child);
        let mut buf = [0u8; 2];
        let got = recv_fds(recv_sock, &mut buf, Some(2), MSG_CMSG_CLOEXEC);
        check!(
            got.n == 1 && got.fds.len() == 2,
            "kind {kind}: {} fds",
            got.fds.len()
        );
        check!(
            sys(1, &[got.fds[1], b"z".as_ptr() as u64, 1]) == 1,
            "kind {kind}: write through the received end"
        );
        let mut byte = [0u8; 1];
        check!(
            sys(0, &[got.fds[0], byte.as_mut_ptr() as u64, 1]) == 1 && byte[0] == b'z',
            "kind {kind}: read it back"
        );
        for fd in got.fds {
            close(fd);
        }
        Ok(())
    };
    round_trip(0)?;
    let (leader_fds, child_fds) = (open_count(p.leader), open_count(p.child));
    let pipes = pipe::Pipe::live();
    let frames = crate::mem::frame_stats().live();
    for round in 0..3000usize {
        round_trip(round)?;
        if round % 500 == 0 {
            check!(
                pipe::in_flight() == 0,
                "round {round}: {} in flight",
                pipe::in_flight()
            );
        }
    }
    check!(
        open_count(p.leader) == leader_fds && open_count(p.child) == child_fds,
        "descriptors leaked: leader {} -> {}, child {} -> {}",
        leader_fds,
        open_count(p.leader),
        child_fds,
        open_count(p.child)
    );
    check!(
        pipe::Pipe::live() == pipes,
        "pipes {} -> {}",
        pipes,
        pipe::Pipe::live()
    );
    check!(pipe::in_flight() == 0, "{} in flight", pipe::in_flight());
    let frames_after = crate::mem::frame_stats().live();
    check!(frames_after == frames, "frames {frames} -> {frames_after}");
    task::harness::reset();
    Ok(())
}

/// A socket end sent through its own pair, or through a pair it carries, is
/// refused with `ETOOMANYREFS`: those edges close a cycle of sockets carrying
/// each other, which nothing would read or purge (there is no garbage
/// collector), leaking the in-flight budget. A pair carrying only a pipe end
/// still travels (`reach.rs` has the longer cases).
pub fn scmrights_cycles_refused() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    check!(send_fds(p.a, b"s", &[p.a]) == ETOOMANYREFS, "a through a");
    check!(send_fds(p.a, b"s", &[p.b]) == ETOOMANYREFS, "b through a");
    check!(
        send_fds(p.a, b"s", &[p.w, p.b]) == ETOOMANYREFS,
        "b beside a pipe end through a"
    );
    check!(
        pipe::in_flight() == 0,
        "a refusal left {} descriptors in flight",
        pipe::in_flight()
    );
    // An idle pair travels; the carrying pair is then pinned until read.
    check!(
        send_fds(p.a, b"c", &[p.sa]) == 1,
        "an idle seqpacket end through a"
    );
    check!(
        send_fds(p.sa, b"x", &[p.b]) == ETOOMANYREFS,
        "an end of a carrying pair through sa"
    );
    check!(
        send_fds(p.sa, b"x", &[p.a]) == ETOOMANYREFS,
        "the other end of a carrying pair through sa"
    );
    check!(
        send_fds(p.sa, b"y", &[p.w]) == 1,
        "a pipe end through sa still travels"
    );
    check!(
        send_fds(p.a, b"c", &[p.sb]) == 1,
        "the other end of sa, which carries a pipe end, through a"
    );
    let mut buf = [0u8; 4];
    let got = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && got.fds.len() == 1,
        "receive the carried end: n={:#x} fds={}",
        got.n,
        got.fds.len()
    );
    close(got.fds[0]);
    let got = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(got.fds.len() == 1, "receive the end of sa that a carried");
    close(got.fds[0]);
    check!(send_fds(p.sa, b"x", &[p.b]) == 1, "an idle pair through sa");
    for _ in 0..2 {
        let got = recv_fds(p.sb, &mut buf, Some(4), 0);
        check!(got.fds.len() == 1, "receive what sa carried");
        close(got.fds[0]);
    }
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

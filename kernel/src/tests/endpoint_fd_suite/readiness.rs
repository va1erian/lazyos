//! What an endpoint descriptor reports, and when it hangs up.

use super::*;

const IN_OUT: u16 = POLLIN | POLLOUT;

/// Readable on a queued message, writable while the peer has room, and
/// readable with a hang-up once the peer closed.
pub fn readiness() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let fd = watch(b)?;
    check!(
        revents(fd, IN_OUT) == POLLOUT,
        "a fresh endpoint: {:#x}",
        revents(fd, IN_OUT)
    );
    let message = message()?;
    send(a, &message)?;
    check!(
        revents(fd, IN_OUT) == IN_OUT,
        "a queued message: {:#x}",
        revents(fd, IN_OUT)
    );
    check!(revents(fd, POLLOUT) == POLLOUT, "POLLIN reported unasked");
    check!(take(b)?, "nothing to take");
    check!(
        revents(fd, IN_OUT) == POLLOUT,
        "drained: {:#x}",
        revents(fd, IN_OUT)
    );
    // Fill the peer's inbox from this side: no room, no POLLOUT.
    let mut sent = 0;
    let refusal = loop {
        match channels::send(b, &message) {
            Ok(()) => sent += 1,
            Err(error) => break error,
        }
    };
    check!(
        refusal == channels::Error::QueueFull,
        "after {sent} messages the peer refused with {refusal:?}, not a full queue"
    );
    check!(
        revents(fd, IN_OUT) == 0,
        "full peer: {:#x}",
        revents(fd, IN_OUT)
    );
    check!(take(a)?, "the peer had nothing");
    check!(
        revents(fd, IN_OUT) == POLLOUT,
        "room again: {:#x}",
        revents(fd, IN_OUT)
    );
    send(a, &message)?;
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    let closed = POLLIN | POLLHUP;
    check!(
        revents(fd, IN_OUT) == closed,
        "peer closed with mail: {:#x}",
        revents(fd, IN_OUT)
    );
    check!(take(b)?, "the last message was lost");
    check!(
        revents(fd, IN_OUT) == closed,
        "peer closed, drained: {:#x}",
        revents(fd, IN_OUT)
    );
    close(fd)?;
    channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
    nothing_left("readiness")
}

/// The descriptor never outlives its handle: closing the handle, releasing
/// it while another holder keeps the side, or the channel vanishing all
/// hang it up, and a new handle to the same side does not revive it.
pub fn hangs_up_with_its_handle() -> Result<(), String> {
    fresh()?;
    let gone = POLLHUP | POLLERR;
    // Closing the watched handle.
    let (a, b) = pair()?;
    let fd = watch(b)?;
    channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
    check!(
        revents(fd, IN_OUT) == gone,
        "closed handle: {:#x}",
        revents(fd, IN_OUT)
    );
    close(fd)?;
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    // Releasing it while a second handle keeps the side open.
    let (a, b) = pair()?;
    let entry = handles::get(b).map_err(|e| format!("{e:?}"))?;
    let other =
        handles::open(entry.kind, entry.rights, entry.object_id).map_err(|e| format!("{e:?}"))?;
    let fd = watch(b)?;
    channels::release_endpoint(b).map_err(|e| format!("{e:?}"))?;
    check!(
        revents(fd, IN_OUT) == gone,
        "released handle: {:#x}",
        revents(fd, IN_OUT)
    );
    let still = watch(other)?;
    check!(
        revents(still, IN_OUT) == POLLOUT,
        "the other holder's side: {:#x}",
        revents(still, IN_OUT)
    );
    send(a, &message()?)?;
    check!(
        revents(fd, IN_OUT) == gone,
        "a message revived a released watch"
    );
    check!(
        revents(still, POLLIN) == POLLIN,
        "the other holder missed the message"
    );
    close(fd)?;
    close(still)?;
    // Both sides gone: the channel no longer exists.
    channels::close_endpoint(other).map_err(|e| format!("{e:?}"))?;
    let fd = watch(a)?;
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    check!(
        revents(fd, IN_OUT) == gone,
        "vanished channel: {:#x}",
        revents(fd, IN_OUT)
    );
    close(fd)?;
    nothing_left("hangs_up")
}

/// Descriptor first or handle first, inside an epoll set or not: every order
/// releases everything, and an epoll interest goes with its descriptor.
pub fn close_ordering() -> Result<(), String> {
    fresh()?;
    let epfd = epoll_create()?;
    for round in 0..4u64 {
        let (a, b) = pair()?;
        let fd = watch(b)?;
        if round & 1 != 0 {
            epoll_add(epfd, fd, POLLIN as u32)?;
        }
        if round & 2 != 0 {
            channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
            if round & 1 != 0 {
                let ready = epoll_wait(epfd, 0)?;
                let gone = u32::from(POLLHUP | POLLERR);
                check!(
                    ready == [(gone, fd)],
                    "round {round}: epoll saw {ready:?} for a closed handle"
                );
            }
            close(fd)?;
        } else {
            close(fd)?;
            channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
        }
        check!(
            epoll_wait(epfd, 0)?.is_empty(),
            "round {round}: a closed fd still reported"
        );
        channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
        check!(
            endpointfd::live() == 0,
            "round {round}: a watch outlived its descriptor"
        );
    }
    close(epfd)?;
    nothing_left("close_ordering")
}

/// `EPOLLET`: one report per arrival, even while the endpoint stays
/// readable; level-triggered interests report every time.
pub fn epoll_edge_rearm() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (edge, level) = (watch(b)?, watch(b)?);
    let epfd = epoll_create()?;
    epoll_add(epfd, edge, POLLIN as u32 | EPOLLET)?;
    epoll_add(epfd, level, POLLIN as u32)?;
    check!(
        epoll_wait(epfd, 0)?.is_empty(),
        "an empty endpoint reported"
    );
    let message = message()?;
    let pollin = u32::from(POLLIN);
    for arrival in 1..=3 {
        send(a, &message)?;
        let mut ready = epoll_wait(epfd, 0)?;
        ready.sort_unstable_by_key(|&(_, data)| data);
        let mut expected = alloc::vec![(pollin, edge), (pollin, level)];
        expected.sort_unstable_by_key(|&(_, data)| data);
        check!(ready == expected, "arrival {arrival}: {ready:?}");
        let again = epoll_wait(epfd, 0)?;
        check!(
            again == [(pollin, level)],
            "arrival {arrival}, no news: {again:?}"
        );
    }
    while take(b)? {}
    check!(
        epoll_wait(epfd, 0)?.is_empty(),
        "a drained endpoint reported"
    );
    let del = event(0, 0);
    let ret = sys(
        SYS_EPOLL_CTL,
        [epfd, EPOLL_CTL_DEL, level, del.as_ptr() as u64],
    );
    check!(ret == 0, "epoll_ctl(DEL) returned {ret:#x}");
    for fd in [edge, level, epfd] {
        close(fd)?;
    }
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
    nothing_left("epoll_edge_rearm")
}

/// The descriptor is not a stream: `read`, `write` and `lseek` fail and take
/// nothing from the endpoint.
pub fn rejects_io() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let fd = watch(b)?;
    send(a, &message()?)?;
    let mut buf = [0u8; 64];
    let einval = (22u64).wrapping_neg();
    let read = sys(0, [fd, buf.as_mut_ptr() as u64, buf.len() as u64, 0]);
    check!(read == einval, "read gave {read:#x}");
    let write = sys(1, [fd, buf.as_ptr() as u64, 1, 0]);
    check!(write == einval, "write gave {write:#x}");
    let seek = sys(8, [fd, 0, 0, 0]);
    check!(seek == (29u64).wrapping_neg(), "lseek gave {seek:#x}");
    check!(take(b)?, "the descriptor consumed the message");
    close(fd)?;
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
    nothing_left("rejects_io")
}

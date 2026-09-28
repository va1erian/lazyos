//! `epoll`: level/edge triggers, hangup, the `maxevents` boundary,
//! starvation, and an add/wait soak.

use super::*;

/// `epoll`: level trigger, zero timeout, `EPOLLET` edges, and `EPOLLHUP`.
pub fn epoll_level_edge_hangup() -> Result<(), String> {
    fresh()?;
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (r, w) = (fds[0] as u64, fds[1] as u64);
    let epfd = process::linux::dispatch_for_test(291, SOCK_CLOEXEC, 0, 0);
    check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");

    let interest = epoll_event(EPOLLIN, 0x1234);
    check!(
        epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == 0,
        "ADD failed"
    );
    check!(
        epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == EEXIST,
        "duplicate ADD not rejected"
    );
    let mut out = [0u8; 24];
    check!(epoll_wait0(epfd, &mut out) == 0, "idle epoll_wait woke");

    check!(write_fd(w, b"a") == 1, "pipe write");
    check!(
        epoll_wait0(epfd, &mut out) == 1,
        "readable pipe not reported"
    );
    let (events, data) = unpack_event(&out);
    check!(events & EPOLLIN != 0, "missing EPOLLIN: {events:#x}");
    check!(data == 0x1234, "user data lost: {data:#x}");
    // Level trigger: still ready while the byte sits undrained.
    check!(
        epoll_wait0(epfd, &mut out) == 1,
        "level trigger drained early"
    );

    // Edge trigger: one report, then silence until the stream changes.
    let edge = epoll_event(EPOLLIN | EPOLLET, 0x9);
    check!(epoll_ctl(epfd, EPOLL_CTL_MOD, r, &edge) == 0, "MOD failed");
    check!(
        epoll_wait0(epfd, &mut out) == 1,
        "edge did not report on arm"
    );
    check!(
        epoll_wait0(epfd, &mut out) == 0,
        "edge repeated without a change"
    );
    let mut one = [0u8; 1];
    check!(read_fd(r, &mut one) == 1, "drain read");
    check!(write_fd(w, b"b") == 1, "second pipe write");
    check!(
        epoll_wait0(epfd, &mut out) == 1,
        "new data did not re-arm the edge"
    );

    // Hangup: the write end closes, so the interest reports EPOLLHUP.
    check!(task::fd_close(w as usize), "close write end failed");
    let level = epoll_event(EPOLLIN, 0x77);
    check!(
        epoll_ctl(epfd, EPOLL_CTL_MOD, r, &level) == 0,
        "MOD for HUP failed"
    );
    check!(epoll_wait0(epfd, &mut out) == 1, "hangup not reported");
    let (events, data) = unpack_event(&out);
    check!(events & EPOLLHUP != 0, "missing EPOLLHUP: {events:#x}");
    check!(data == 0x77, "hangup data lost");

    check!(epoll_ctl(epfd, EPOLL_CTL_DEL, r, &level) == 0, "DEL failed");
    check!(
        epoll_wait0(epfd, &mut out) == 0,
        "deleted interest still ready"
    );
    check!(
        epoll_ctl(epfd, EPOLL_CTL_DEL, r, &level) == ENOENT,
        "duplicate DEL not rejected"
    );

    check!(task::fd_close(r as usize), "close read end failed");
    check!(task::fd_close(epfd as usize), "close epoll failed");
    check!(fds_clean(), "epoll test left a descriptor");
    check!(pipe::Pipe::live() == 0, "the pipe was not freed");
    Ok(())
}

/// `EPOLLET` edges that are ready but beyond `maxevents` stay pending: each
/// later wait reports the next one instead of silently marking it seen.
pub fn epoll_edge_over_maxevents() -> Result<(), String> {
    fresh()?;
    let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
    check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
    let mut ends: Vec<(u64, u64)> = Vec::new();
    for tag in 1..=3u64 {
        let mut fds = [0i32; 2];
        check!(
            process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
            "pipe {tag} failed"
        );
        let (r, w) = (fds[0] as u64, fds[1] as u64);
        let edge = epoll_event(EPOLLIN | EPOLLET, tag);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, r, &edge) == 0,
            "ADD {tag} failed"
        );
        check!(write_fd(w, b"x") == 1, "pipe {tag} write");
        ends.push((r, w));
    }
    let mut one = [0u8; 12];
    let mut seen: Vec<u64> = Vec::new();
    for round in 0..3 {
        check!(
            epoll_wait0(epfd, &mut one) == 1,
            "round {round}: a pending edge was lost past maxevents"
        );
        let (_, data) = unpack_event(&one);
        check!(
            !seen.contains(&data),
            "round {round}: edge {data} reported twice"
        );
        seen.push(data);
    }
    check!(
        epoll_wait0(epfd, &mut one) == 0,
        "an edge repeated after every edge was reported"
    );
    for (r, w) in ends {
        check!(task::fd_close(r as usize), "close read end failed");
        check!(task::fd_close(w as usize), "close write end failed");
    }
    check!(task::fd_close(epfd as usize), "close epoll failed");
    check!(fds_clean(), "epoll edge test left a descriptor");
    check!(pipe::Pipe::live() == 0, "a pipe was not freed");
    Ok(())
}

/// With `maxevents = 1`, a level-triggered interest that stays ready must
/// not starve interests registered after it: successive waits rotate
/// through the ready set, as Linux does.
pub fn epoll_level_does_not_starve() -> Result<(), String> {
    fresh()?;
    let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
    check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
    let mut ends: Vec<(u64, u64)> = Vec::new();
    for (tag, events) in [(1u64, EPOLLIN), (2, EPOLLIN | EPOLLET), (3, EPOLLIN)] {
        let mut fds = [0i32; 2];
        check!(
            process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
            "pipe {tag} failed"
        );
        let (r, w) = (fds[0] as u64, fds[1] as u64);
        let interest = epoll_event(events, tag);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == 0,
            "ADD {tag} failed"
        );
        check!(write_fd(w, b"x") == 1, "pipe {tag} write");
        ends.push((r, w));
    }
    let mut one = [0u8; 12];
    let mut seen: Vec<u64> = Vec::new();
    for round in 0..6 {
        check!(
            epoll_wait0(epfd, &mut one) == 1,
            "round {round}: nothing reported"
        );
        let (_, data) = unpack_event(&one);
        if !seen.contains(&data) {
            seen.push(data);
        }
    }
    seen.sort();
    check!(
        seen == [1, 2, 3],
        "maxevents=1 waits starved an interest: saw {seen:?}"
    );
    for (r, w) in ends {
        check!(task::fd_close(r as usize), "close read end failed");
        check!(task::fd_close(w as usize), "close write end failed");
    }
    check!(task::fd_close(epfd as usize), "close epoll failed");
    check!(fds_clean(), "epoll starvation test left a descriptor");
    check!(pipe::Pipe::live() == 0, "a pipe was not freed");
    Ok(())
}

/// Soak: thousands of `epoll_ctl` add/mod/del cycles over mixed targets,
/// and a full interest set, with no descriptor, pipe or interest leak.
pub fn epoll_soak_add_wait_cycles() -> Result<(), String> {
    fresh()?;
    let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
    check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
    let efd = process::linux::dispatch_for_test(290, 0, O_NONBLOCK, 0);
    check!((efd as i64) > 0, "eventfd2 returned {efd:#x}");
    let mut fds = [0i32; 2];
    check!(
        process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
        "pipe failed"
    );
    let (r, w) = (fds[0] as u64, fds[1] as u64);
    let mut out = [0u8; 12];
    let interest = epoll_event(EPOLLIN, 0);
    for round in 0..20_000u64 {
        let target = if round % 2 == 0 { efd } else { r };
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, target, &interest) == 0,
            "round {round}: ADD failed"
        );
        let _ = epoll_wait0(epfd, &mut out);
        let changed = epoll_event(EPOLLIN, round);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_MOD, target, &changed) == 0,
            "round {round}: MOD failed"
        );
        check!(
            epoll_ctl(epfd, EPOLL_CTL_DEL, target, &interest) == 0,
            "round {round}: DEL failed"
        );
    }

    // A full interest set: eight eventfds, all made ready at once.
    let mut events: Vec<u64> = Vec::new();
    for value in 1..=8u64 {
        let fd = process::linux::dispatch_for_test(290, value, O_NONBLOCK, 0);
        check!((fd as i64) > 0, "eventfd {value} returned {fd:#x}");
        let interest = epoll_event(EPOLLIN, value);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &interest) == 0,
            "ADD eventfd {value} failed"
        );
        events.push(fd);
    }
    for fd in &events {
        let one = 1u64.to_le_bytes();
        check!(write_fd(*fd, &one) == 8, "eventfd write failed");
    }
    let mut ready = [0u8; 12 * 8];
    check!(
        epoll_wait0(epfd, &mut ready) == 8,
        "not all interests ready"
    );
    let mut seen = [false; 9];
    for index in 0..8 {
        let (bits, data) = unpack_event(&ready[index * 12..]);
        check!(bits & EPOLLIN != 0, "ready event {index} lacks EPOLLIN");
        seen[data as usize] = true;
    }
    check!(
        (1..=8).all(|value| seen[value]),
        "ready data values lost: {seen:?}"
    );

    // Closing a registered descriptor drops its interest.
    check!(task::fd_close(events[0] as usize), "close eventfd failed");
    check!(
        epoll_ctl(epfd, EPOLL_CTL_DEL, events[0], &interest) == ENOENT,
        "closed descriptor kept its interest"
    );
    for fd in events.iter().skip(1) {
        check!(task::fd_close(*fd as usize), "close eventfd failed");
        let _ = epoll_ctl(epfd, EPOLL_CTL_DEL, *fd, &interest);
    }
    check!(task::fd_close(efd as usize), "close eventfd failed");
    check!(task::fd_close(epfd as usize), "close epoll failed");
    for fd in [r, w] {
        check!(task::fd_close(fd as usize), "close pipe fd failed");
    }
    check!(fds_clean(), "epoll soak leaked a descriptor");
    check!(pipe::Pipe::live() == 0, "epoll soak leaked a pipe");
    Ok(())
}

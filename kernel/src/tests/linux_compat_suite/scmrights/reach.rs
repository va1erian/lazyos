//! The exact cycle rule (`ipc::pipe::reach`, issue #865): a pair whose queue
//! already holds descriptors travels unless they lead back to where it is
//! sent. WebKit's NetworkProcess queues a descriptor on a new connection
//! while the connection's other end is on its way to the UI process, which
//! forwards it to the WebProcess; refusing that forward left the start page
//! hanging for good.

use super::*;

/// The NetworkProcess's pattern: `(n_server, n_client)` is a new connection,
/// `ui` the pair it travels through. Returns `(server, client)`.
fn new_connection_with_handle(ui: u64, handle: u64) -> Result<(u64, u64), String> {
    let (server, client) = seqpair()?;
    check!(
        send_fds(server, b"h", &[handle]) == 1,
        "the server end sends a handle on the new connection"
    );
    check!(
        send_fds(ui, b"c", &[client]) == 1,
        "the client end of a connection that holds a handle travels"
    );
    Ok((server, client))
}

/// Receive what `sock`'s next message carries: exactly one descriptor.
fn take_one(sock: u64, what: &str) -> Result<u64, String> {
    let mut buf = [0u8; 4];
    let got = recv_fds(sock, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && got.fds.len() == 1,
        "{what}: n={:#x} fds={}",
        got.n,
        got.fds.len()
    );
    Ok(got.fds[0])
}

/// A carrying pair travels when nothing it carries leads back: the client end
/// of a connection whose queue holds a pipe end reaches the other process,
/// and the pipe end arrives through it. A chain of five pairs, each carrying
/// the next, refuses the edge that would close it (from the far end back to
/// the first), allows the same end through an unrelated pair, and two
/// carried ends leading to one pair are not a cycle.
pub fn scmrights_carrying_pair_travels() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let (server, client) = new_connection_with_handle(p.sa, p.r)?;
    let forwarded = take_one(p.sb, "the forwarded client end")?;
    let handle = take_one(forwarded, "the handle through the forwarded end")?;
    check!(send_fds(server, b"x", &[]) == 1, "data on the server end");
    let mut buf = [0u8; 4];
    let got = recv_fds(forwarded, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && got.fds.is_empty() && buf[0] == b'x',
        "the forwarded end is the connection: n={:#x} fds={}",
        got.n,
        got.fds.len()
    );
    for fd in [server, client, forwarded, handle] {
        close(fd);
    }

    // P0 carries P1, ..., P3 carries P4.
    let mut chain = Vec::new();
    for _ in 0..5 {
        chain.push(seqpair()?);
    }
    for i in 0..4 {
        check!(
            send_fds(chain[i].0, b"n", &[chain[i + 1].0]) == 1,
            "P{i} carries P{}",
            i + 1
        );
    }
    check!(
        send_fds(chain[4].0, b"z", &[chain[0].1]) == ETOOMANYREFS,
        "an end of P0 through P4 closes a cycle of five"
    );
    check!(
        send_fds(chain[2].0, b"z", &[chain[1].0]) == ETOOMANYREFS,
        "an end of P1 through P2 closes a cycle of two"
    );
    check!(
        pipe::in_flight() == 4,
        "refusals left {} in flight, want the chain's 4",
        pipe::in_flight()
    );
    check!(
        send_fds(p.a, b"z", &[chain[0].1]) == 1,
        "the same end of P0 through an unrelated pair"
    );
    // P0 is now reached from (a, b) and P3 from P2: a second way into P3
    // (through P0's chain and directly) is no cycle.
    check!(
        send_fds(p.a, b"z", &[chain[3].1]) == 1,
        "a second carried end leading into the chain"
    );
    check!(
        send_fds(chain[4].0, b"z", &[p.b]) == ETOOMANYREFS,
        "an end of (a, b) through P4, which (a, b) now reaches"
    );
    for (x, y) in chain {
        close(x);
        close(y);
    }
    // What (a, b) carries goes when the pair does: the chain with it.
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

/// 300 rounds of WebKit's start-up: a new connection holding a handle, its
/// client end forwarded twice, the handle read through it, and the cycle the
/// new connection could close refused every round. Nothing is left in flight,
/// no pipe and no frame leaks.
pub fn scmrights_carrying_pair_soak() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let frames = crate::mem::frame_stats().live();
    for round in 0..300u32 {
        let (server, client) = new_connection_with_handle(p.sa, p.w)?;
        // The UI process forwards what it got to the WebProcess.
        let at_ui = take_one(p.sb, "the client end at the UI")?;
        check!(
            send_fds(p.a, b"f", &[at_ui]) == 1,
            "round {round}: the forward of a carrying end"
        );
        let at_web = take_one(p.b, "the client end at the WebProcess")?;
        check!(
            send_fds(server, b"z", &[at_web]) == ETOOMANYREFS,
            "round {round}: the client end back through its own connection"
        );
        let handle = take_one(at_web, "the handle")?;
        for fd in [server, client, at_ui, at_web, handle] {
            close(fd);
        }
        check!(
            pipe::in_flight() == 0,
            "round {round}: {} descriptors in flight",
            pipe::in_flight()
        );
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
    let frames_after = crate::mem::frame_stats().live();
    check!(frames_after <= frames, "frames {frames} -> {frames_after}");
    settled(pipes_before)
}

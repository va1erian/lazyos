//! The ownership check of `netctl sockprobe=1`: a second task must be refused
//! on every call that names the first task's socket.
//!
//! The parent opens a socket and starts `netctl sockowner=<id>`; that child is
//! the other task. `netd` keys a socket by the sender's task slot *and* pid, so
//! the child, in a different slot, is a stranger to it.

use alloc::format;
use alloc::string::String;

use user::messenger::errno;
use user::messenger::netsock::{wire, Addr, Client};
use user::sys;

use super::common::{fail, is_errno};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];

/// Open a socket, run the second task against it, and say whether every call
/// the second task made was refused (its exit status is 0). The owner must
/// still be able to use its socket afterwards.
pub(super) fn second_task_is_refused(client: &Client) -> Result<bool, String> {
    let mine = client
        .open(wire::SOCK_KIND_DATAGRAM)
        .map_err(fail("open"))?;
    client.bind(mine, Addr::ANY).map_err(fail("bind"))?;
    let cmdline = format!("NETCTL.ELF sockowner={mine}\0");
    let pid = sys::spawn(cmdline.as_bytes())
        .ok_or_else(|| String::from("cannot start the second task"))?;
    let deadline = sys::clock() + 1500;
    let mut status = None;
    while status.is_none() && sys::clock() < deadline {
        if let Some((done, code)) = sys::wait(deadline) {
            if done == pid {
                status = Some(code);
            }
        }
    }
    client
        .local_addr(mine)
        .map_err(fail("the owner still uses its socket"))?;
    client.close(mine).map_err(fail("close"))?;
    Ok(status == Some(0))
}

/// The second task of the ownership check: every call on `sock`, which
/// belongs to its parent, must be refused with `EACCES`. Returns the number
/// of refusals seen.
pub(super) fn foreign_owner(client: &Client, sock: u32) -> Result<u32, String> {
    let mut seen = 0;
    let mut expect = |name: &str, refused: bool| -> Result<(), String> {
        if refused {
            seen += 1;
            Ok(())
        } else {
            Err(format!("a foreign {name} was not refused with EACCES"))
        }
    };
    expect("Close", is_errno(&client.close(sock), errno::EACCES))?;
    expect(
        "Send",
        is_errno(&client.send(sock, b"x", 100), errno::EACCES),
    )?;
    expect("Recv", is_errno(&client.recv(sock, 10, 100), errno::EACCES))?;
    expect(
        "RecvFrom",
        is_errno(&client.recv_from(sock, 10, 100), errno::EACCES),
    )?;
    expect(
        "SendTo",
        is_errno(
            &client.send_to(sock, Addr::new(GATEWAY, 9), b"x"),
            errno::EACCES,
        ),
    )?;
    expect("Poll", is_errno(&client.poll(sock, 1, 100), errno::EACCES))?;
    expect(
        "Bind",
        is_errno(&client.bind(sock, Addr::ANY), errno::EACCES),
    )?;
    expect("Listen", is_errno(&client.listen(sock, 1), errno::EACCES))?;
    expect("Accept", is_errno(&client.accept(sock, 100), errno::EACCES))?;
    expect(
        "Shutdown",
        is_errno(&client.shutdown(sock, wire::SHUTDOWN_BOTH), errno::EACCES),
    )?;
    expect(
        "LocalAddr",
        is_errno(&client.local_addr(sock), errno::EACCES),
    )?;
    expect("PeerAddr", is_errno(&client.peer_addr(sock), errno::EACCES))?;
    Ok(seen)
}

//! Parked socket calls: trying a call, keeping it when it cannot finish,
//! answering it later (or at its deadline), and reclaiming what a dead owner
//! left behind. The dispatch that creates them is in `sock.rs`.

use alloc::string::String;
use alloc::vec::Vec;

use netstack::{ready, SockError};
use user::messenger::netsock::{self as api, errno as sock_errno, wire};
use user::messenger::{errno, services, Endpoint, Message, Parcel};
use user::sys;

use super::service::Netd;
use super::sock::{err, error_parcel, reply, wire_addr, Result};

/// Parked socket calls one owner may have at once.
const PER_OWNER_PARKED: usize = 4;
/// Parked socket calls, all owners together.
const TOTAL_PARKED: usize = 32;

/// What a parked call is waiting to do.
pub(super) enum Op {
    Connect,
    Accept,
    Send(Vec<u8>),
    Recv(usize),
    RecvFrom(usize),
    Poll(u32),
}

pub(super) struct Parked {
    txn: u64,
    owner: u64,
    sock: u32,
    method: u32,
    deadline_ms: i64,
    op: Op,
}

impl Netd {
    /// Try the call now; if it cannot finish, park it (within the limits).
    pub(super) fn park(
        &mut self,
        message: &Message,
        owner: u64,
        sock: u32,
        op: Op,
        wait: i64,
        now_ms: i64,
    ) -> Result<Option<Parcel>> {
        // A reply needs a transaction; a one-way call has nobody to answer.
        let Some(txn) = message.txn else {
            return Err(err(errno::EINVAL));
        };
        let call = Parked {
            txn,
            owner,
            sock,
            method: message.method(),
            deadline_ms: now_ms + wait,
            op,
        };
        if let Some(parcel) = self.attempt(&call)? {
            return Ok(Some(parcel));
        }
        let mine = self
            .parked_socks
            .iter()
            .filter(|p| p.owner == owner)
            .count();
        if mine >= PER_OWNER_PARKED || self.parked_socks.len() >= TOTAL_PARKED {
            return Err(err(errno::EAGAIN));
        }
        self.sock_stats.parked += 1;
        self.parked_socks.push(call);
        Ok(None)
    }

    /// Try a call against the socket as it is now: `Ok(Some)` is the reply,
    /// `Ok(None)` means keep waiting.
    fn attempt(&mut self, call: &Parked) -> Result<Option<Parcel>> {
        let (sock, owner, method) = (call.sock, call.owner, call.method);
        let ok = |body: core::result::Result<Vec<u8>, libmessenger::Error>| {
            reply(method, body).map(Some)
        };
        match &call.op {
            Op::Connect => match self.net.socket_connect_status(sock, owner) {
                Ok(true) => Ok(Some(api::parcel(method, Vec::new()))),
                Ok(false) => Ok(None),
                Err(error) => Err(self.sock_error(error)),
            },
            Op::Accept => match self.net.socket_accept(sock, owner) {
                Ok(Some((conn, peer))) => ok(wire::encode_accept_reply(&wire::AcceptReply {
                    conn,
                    peer: wire_addr(peer),
                })),
                Ok(None) => Ok(None),
                Err(error) => Err(self.sock_error(error)),
            },
            Op::Send(data) => match self.net.socket_send(sock, owner, data) {
                Ok(sent) => ok(wire::encode_send_reply(&wire::SendReply {
                    sent: sent as u32,
                })),
                Err(SockError::WouldBlock) => Ok(None),
                Err(error) => Err(self.sock_error(error)),
            },
            Op::Recv(max) => match self.net.socket_recv(sock, owner, *max) {
                Ok(Some(data)) => ok(wire::encode_recv_reply(&wire::RecvReply { data })),
                Ok(None) | Err(SockError::WouldBlock) => Ok(None),
                Err(error) => Err(self.sock_error(error)),
            },
            Op::RecvFrom(max) => match self.net.socket_recvfrom(sock, owner, *max) {
                Ok(Some((data, from))) => ok(wire::encode_recv_from_reply(&wire::RecvFromReply {
                    data,
                    from: wire_addr(from),
                })),
                Ok(None) => Ok(None),
                Err(error) => Err(self.sock_error(error)),
            },
            Op::Poll(interest) => {
                let bits = self
                    .net
                    .socket_readiness(sock, owner)
                    .map_err(|e| self.sock_error(e))?;
                // Closed and error conditions are reported whether asked for or not.
                let wanted = interest | ready::CLOSED | ready::ERROR;
                match bits & wanted {
                    0 => Ok(None),
                    hit => ok(wire::encode_poll_reply(&wire::PollReply { ready: hit })),
                }
            }
        }
    }

    /// Answer the parked calls that can finish, and the ones past their
    /// deadline. Called after every `poll` of the stack.
    pub(super) fn service_parked(&mut self, server: &Endpoint, now_ms: i64) {
        if self.parked_socks.is_empty() {
            return;
        }
        let calls = core::mem::take(&mut self.parked_socks);
        for call in calls {
            let answer = match self.attempt(&call) {
                Ok(None) if now_ms >= call.deadline_ms => {
                    self.sock_stats.park_timeouts += 1;
                    Some(error_parcel(call.method, errno::ETIMEDOUT))
                }
                Ok(None) => None,
                Ok(Some(parcel)) => Some(parcel),
                Err(error) => Some(services::error_reply(api::INTERFACE, call.method, error)),
            };
            match answer {
                // A caller that gave up or died has no transaction left: not a fault.
                Some(parcel) => {
                    let _ = server.reply_or_drop(call.txn, &parcel);
                }
                None => self.parked_socks.push(call),
            }
        }
    }

    /// The nearest deadline among the parked calls, for the event loop's wait.
    pub(super) fn parked_delay_ms(&self, now_ms: i64) -> Option<u64> {
        self.parked_socks
            .iter()
            .map(|p| (p.deadline_ms - now_ms).max(0) as u64)
            .min()
    }

    /// A socket was closed: whoever was waiting on it learns it is gone.
    pub(super) fn drop_calls_on(&mut self, owner: u64, sock: u32) {
        let (gone, kept): (Vec<Parked>, Vec<Parked>) = core::mem::take(&mut self.parked_socks)
            .into_iter()
            .partition(|p| p.owner == owner && p.sock == sock);
        self.parked_socks = kept;
        for call in gone {
            let parcel = error_parcel(call.method, sock_errno::EBADF);
            self.outbox.push((call.txn, parcel));
        }
    }

    /// Reclaim the sockets of owners that exited. Returns how many sockets.
    pub(super) fn sweep_owners(&mut self, tick: u64, now_ms: i64) -> usize {
        let mut reclaimed = 0;
        for owner in self.net.socket_owners() {
            // Sockets of Linux programs have owners of their own (the kernel
            // frees those when the application closes them), not task owners.
            if owner >= super::inet::OWNER_BASE || self.tasks.alive(owner, tick) {
                continue;
            }
            let count = self.net.sockets_close_owner(owner, now_ms);
            self.parked_socks.retain(|p| p.owner != owner);
            reclaimed += count;
            let note: String = alloc::format!("NETD:RECLAIM owner={owner:#x} sockets={count}\n");
            sys::write_str(&note);
        }
        reclaimed
    }
}

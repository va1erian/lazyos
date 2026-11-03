//! Inbound signalling: connection, configuration, disconnection, echo and
//! information requests, and the answers to ours.
//!
//! Only basic mode is spoken. A peer that asks for retransmission or flow
//! control gets "unacceptable parameters" with basic mode named, which is how
//! it learns to fall back. A configuration the peer refuses ends the channel;
//! there is no renegotiation loop to be driven by a hostile peer.

use alloc::vec;
use alloc::vec::Vec;

use crate::link::{ChanState, Channel, Link, LinkKind};
use crate::manager::{closed, send_signal, violation, Action, CloseReason, Config, Event};
use crate::pdu::{
    conf_result, conn_result, parse_commands, reject, Command, ConfOption, Signal, CID_DYNAMIC_MIN,
    DEFAULT_MTU, MIN_MTU,
};

pub(crate) fn on_payload(
    cfg: &Config,
    link: &mut Link,
    now: u64,
    payload: &[u8],
    out: &mut Vec<Action>,
) {
    match parse_commands(payload) {
        Ok(cmds) => cmds
            .into_iter()
            .for_each(|c| on_command(cfg, link, now, c, out)),
        Err(_) => {
            violation(link, out);
            // The ident of a command we could not parse is unknowable; 1 is
            // the first valid one, and the peer ignores a reject it did not ask for.
            let r = Signal::CommandReject {
                reason: reject::NOT_UNDERSTOOD,
                data: Vec::new(),
            };
            send_signal(link, 1, r, out);
        }
    }
}

fn reject_cid(link: &Link, ident: u8, local: u16, remote: u16, out: &mut Vec<Action>) {
    let mut data = local.to_le_bytes().to_vec();
    data.extend_from_slice(&remote.to_le_bytes());
    send_signal(
        link,
        ident,
        Signal::CommandReject {
            reason: reject::INVALID_CID,
            data,
        },
        out,
    );
}

fn on_command(cfg: &Config, link: &mut Link, now: u64, cmd: Command, out: &mut Vec<Action>) {
    let ident = cmd.ident;
    let br = link.kind == LinkKind::BrEdr;
    match cmd.signal {
        Signal::ConnectionRequest { psm, scid } if br => {
            on_conn_req(cfg, link, now, ident, psm, scid, out)
        }
        Signal::ConnectionResponse {
            dcid, scid, result, ..
        } if br => on_conn_rsp(cfg, link, now, ident, dcid, scid, result, out),
        Signal::ConfigRequest {
            dcid,
            flags,
            options,
        } if br => on_conf_req(cfg, link, now, ident, dcid, flags, &options, out),
        Signal::ConfigResponse { scid, result, .. } if br => {
            on_conf_rsp(link, ident, scid, result, out)
        }
        Signal::DisconnectRequest { dcid, scid } if br => on_disc_req(link, ident, dcid, scid, out),
        Signal::DisconnectResponse { dcid, scid } if br => {
            on_disc_rsp(link, ident, dcid, scid, out)
        }
        Signal::EchoRequest(data) => send_signal(link, ident, Signal::EchoResponse(data), out),
        Signal::InformationRequest { info_type } if br => on_info_req(link, ident, info_type, out),
        Signal::CommandReject { .. } => on_cmd_reject(link, ident, out),
        // We never ask, so any answer is unsolicited and ignored.
        Signal::EchoResponse(_)
        | Signal::InformationResponse { .. }
        | Signal::ConnParamUpdateResponse { .. } => {}
        // A peripheral asks to change the connection parameters; the answer
        // is "rejected" (1) because this host keeps the parameters it chose.
        Signal::ConnParamUpdateRequest { .. } if !br => send_signal(
            link,
            ident,
            Signal::ConnParamUpdateResponse { result: 1 },
            out,
        ),
        _ => {
            let r = Signal::CommandReject {
                reason: reject::NOT_UNDERSTOOD,
                data: Vec::new(),
            };
            send_signal(link, ident, r, out);
        }
    }
}

fn on_info_req(link: &Link, ident: u8, info_type: u16, out: &mut Vec<Action>) {
    // 2: extended features (none), 3: fixed channels (the signalling channel).
    let (result, data) = match info_type {
        2 => (0, vec![0; 4]),
        3 => (0, vec![0x02, 0, 0, 0, 0, 0, 0, 0]),
        _ => (1, Vec::new()),
    };
    send_signal(
        link,
        ident,
        Signal::InformationResponse {
            info_type,
            result,
            data,
        },
        out,
    );
}

fn on_conn_req(
    cfg: &Config,
    link: &mut Link,
    now: u64,
    ident: u8,
    psm: u16,
    scid: u16,
    out: &mut Vec<Action>,
) {
    let refuse = |link: &Link, result: u16, out: &mut Vec<Action>| {
        let sig = Signal::ConnectionResponse {
            dcid: 0,
            scid,
            result,
            status: 0,
        };
        send_signal(link, ident, sig, out);
    };
    if scid < CID_DYNAMIC_MIN {
        return refuse(link, conn_result::INVALID_SOURCE_CID, out);
    }
    if link.remote_in_use(scid) {
        return refuse(link, conn_result::SOURCE_CID_ALLOCATED, out);
    }
    if !cfg.psms.contains(&psm) {
        return refuse(link, conn_result::PSM_NOT_SUPPORTED, out);
    }
    if !link.allowed.contains(&psm) {
        out.push(Action::Event(Event::Refused {
            handle: link.handle,
            psm,
        }));
        return refuse(link, conn_result::SECURITY_BLOCK, out);
    }
    let local = match link.free_cid() {
        Some(c) if link.channels.len() < cfg.max_channels_per_link => c,
        _ => return refuse(link, conn_result::NO_RESOURCES, out),
    };
    let mut ch = Channel::new(local, psm, true, cfg.mtu, now, cfg.rtx_ms);
    ch.remote = scid;
    ch.state = ChanState::Config;
    let sig = Signal::ConnectionResponse {
        dcid: local,
        scid,
        result: conn_result::SUCCESS,
        status: 0,
    };
    send_signal(link, ident, sig, out);
    send_config_request(link, &mut ch, out);
    link.channels.push(ch);
}

fn send_config_request(link: &mut Link, ch: &mut Channel, out: &mut Vec<Action>) {
    ch.ident = link.ident();
    let sig = Signal::ConfigRequest {
        dcid: ch.remote,
        flags: 0,
        options: vec![ConfOption::Mtu(ch.local_mtu)],
    };
    send_signal(link, ch.ident, sig, out);
}

#[allow(clippy::too_many_arguments)]
fn on_conn_rsp(
    cfg: &Config,
    link: &mut Link,
    now: u64,
    ident: u8,
    dcid: u16,
    scid: u16,
    result: u16,
    out: &mut Vec<Action>,
) {
    let handle = link.handle;
    let valid = matches!(link.by_local(scid), Some(c) if c.state == ChanState::WaitConnectRsp && c.ident == ident);
    if !valid {
        return violation(link, out);
    }
    if result == conn_result::PENDING {
        return;
    }
    let bad_dcid =
        result == conn_result::SUCCESS && (dcid < CID_DYNAMIC_MIN || link.remote_in_use(dcid));
    if result != conn_result::SUCCESS || bad_dcid {
        let psm = link.by_local(scid).map_or(0, |c| c.psm);
        link.remove(scid);
        if bad_dcid {
            violation(link, out);
        }
        let result = if bad_dcid { 0xFFFF } else { result };
        out.push(Action::Event(Event::ConnectFailed {
            handle,
            psm,
            result,
        }));
        return;
    }
    let Some(i) = link.channels.iter().position(|c| c.local == scid) else {
        return;
    };
    let mut ch = link.channels.swap_remove(i);
    ch.remote = dcid;
    ch.state = ChanState::Config;
    ch.deadline = now.saturating_add(cfg.rtx_ms);
    send_config_request(link, &mut ch, out);
    link.channels.push(ch);
}

/// Judge the peer's configuration: `(result, options for the response,
/// peer MTU if one was given)`.
fn judge(options: &[ConfOption]) -> (u16, Vec<ConfOption>, Option<u16>) {
    let mut mtu = None;
    let mut unknown = Vec::new();
    for o in options {
        match o {
            ConfOption::Mtu(m) if *m < MIN_MTU => {
                return (
                    conf_result::UNACCEPTABLE,
                    vec![ConfOption::Mtu(DEFAULT_MTU)],
                    None,
                )
            }
            ConfOption::Mtu(m) => mtu = Some(*m),
            ConfOption::Rfc { mode, .. } if *mode != 0 => {
                return (
                    conf_result::UNACCEPTABLE,
                    vec![ConfOption::Rfc {
                        mode: 0,
                        rest: [0; 8],
                    }],
                    None,
                )
            }
            ConfOption::Unknown { .. } if !o.is_hint() => unknown.push(o.clone()),
            _ => {}
        }
    }
    if unknown.is_empty() {
        (conf_result::SUCCESS, Vec::new(), mtu)
    } else {
        (conf_result::UNKNOWN_OPTIONS, unknown, None)
    }
}

#[allow(clippy::too_many_arguments)]
fn on_conf_req(
    cfg: &Config,
    link: &mut Link,
    now: u64,
    ident: u8,
    dcid: u16,
    flags: u16,
    options: &[ConfOption],
    out: &mut Vec<Action>,
) {
    let Some((remote, state)) = link.by_local(dcid).map(|c| (c.remote, c.state)) else {
        violation(link, out);
        return reject_cid(link, ident, dcid, 0, out);
    };
    let respond =
        |link: &Link, flags: u16, result: u16, options: Vec<ConfOption>, out: &mut Vec<Action>| {
            let sig = Signal::ConfigResponse {
                scid: remote,
                flags,
                result,
                options,
            };
            send_signal(link, ident, sig, out);
        };
    match state {
        ChanState::Config => {}
        // Reconfiguration of an open channel is not offered.
        ChanState::Open => return respond(link, 0, conf_result::REJECTED, Vec::new(), out),
        _ => {
            violation(link, out);
            return reject_cid(link, ident, dcid, 0, out);
        }
    }
    let (result, reply, mtu) = judge(options);
    if result != conf_result::SUCCESS {
        return respond(link, 0, result, reply, out);
    }
    let last = flags & 1 == 0;
    if let Some(ch) = link.by_local(dcid) {
        if let Some(m) = mtu {
            ch.peer_mtu = m;
        }
        ch.deadline = now.saturating_add(cfg.rtx_ms);
        // A continuation flag means more options follow in another request.
        ch.remote_done = last;
    }
    respond(
        link,
        u16::from(!last),
        conf_result::SUCCESS,
        Vec::new(),
        out,
    );
    let handle = link.handle;
    if let Some(ch) = link.by_local(dcid) {
        maybe_open(handle, ch, out);
    }
}

fn on_conf_rsp(link: &mut Link, ident: u8, scid: u16, result: u16, out: &mut Vec<Action>) {
    let handle = link.handle;
    let Some(ch) = link.by_local(scid) else {
        return violation(link, out);
    };
    if ch.state != ChanState::Config || ch.ident != ident {
        return violation(link, out);
    }
    if result == conf_result::SUCCESS {
        ch.local_done = true;
        ch.ident = 0;
        return maybe_open(handle, ch, out);
    }
    // The peer wants something else; with one fixed offer there is nothing to
    // renegotiate, so the channel ends.
    let remote = ch.remote;
    link.remove(scid);
    out.push(closed(handle, scid, CloseReason::ConfigFailed));
    let ident = link.ident();
    send_signal(
        link,
        ident,
        Signal::DisconnectRequest { dcid: remote, scid },
        out,
    );
}

fn maybe_open(handle: u16, ch: &mut Channel, out: &mut Vec<Action>) {
    if ch.state == ChanState::Config && ch.local_done && ch.remote_done {
        ch.state = ChanState::Open;
        ch.deadline = u64::MAX;
        out.push(Action::Event(Event::Opened {
            handle,
            cid: ch.local,
            psm: ch.psm,
            incoming: ch.incoming,
            peer_mtu: ch.peer_mtu,
        }));
    }
}

fn on_disc_req(link: &mut Link, ident: u8, dcid: u16, scid: u16, out: &mut Vec<Action>) {
    let handle = link.handle;
    match link.by_local(dcid) {
        Some(c) if c.remote == scid => {}
        _ => return reject_cid(link, ident, dcid, scid, out),
    }
    link.remove(dcid);
    send_signal(link, ident, Signal::DisconnectResponse { dcid, scid }, out);
    out.push(closed(handle, dcid, CloseReason::Peer));
}

fn on_disc_rsp(link: &mut Link, ident: u8, dcid: u16, scid: u16, out: &mut Vec<Action>) {
    let handle = link.handle;
    match link.by_local(scid) {
        Some(c)
            if c.state == ChanState::WaitDisconnectRsp && c.remote == dcid && c.ident == ident =>
        {
            link.remove(scid);
            out.push(closed(handle, scid, CloseReason::Local));
        }
        // After a timeout or a refused configuration the channel is already
        // gone; a late answer is harmless.
        _ => {}
    }
}

fn on_cmd_reject(link: &mut Link, ident: u8, out: &mut Vec<Action>) {
    let handle = link.handle;
    let Some(c) = link
        .channels
        .iter()
        .find(|c| c.ident == ident && ident != 0)
    else {
        return;
    };
    let (cid, psm, connecting) = (c.local, c.psm, c.state == ChanState::WaitConnectRsp);
    link.remove(cid);
    if connecting {
        out.push(Action::Event(Event::ConnectFailed {
            handle,
            psm,
            result: 0xFFFF,
        }));
    } else {
        out.push(closed(handle, cid, CloseReason::Rejected));
    }
}

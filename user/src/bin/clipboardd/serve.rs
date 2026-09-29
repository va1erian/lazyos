//! Service entry point: the serve loop, credential lookup and request
//! dispatch.

use alloc::format;
use user::messenger::{self, clipboard as wire, errno, registry, Error, Message, Parcel};
use user::sys;

use super::args::{demo_from_args, history_from_args, spawn_demo};
use super::state::Clipboard;
use super::POLL_TICKS;

/// Register the service and serve offers, pastes and the changed topic.
pub(super) fn run() -> messenger::Result<()> {
    let history = history_from_args();
    let (published, server) = messenger::create_pair()?;
    registry::register(wire::NAME, &published, &[wire::INTERFACE], 0)?;
    let mut clipboard = Clipboard::new(history);
    sys::write_str(&format!("CLIPBOARD:HISTORY:{history}\n"));
    sys::write_str("CLIPBOARD:READY\n");
    // The demo pair (if requested) starts right here, so its two ELF loads
    // complete before the supervisor's crash test files its short-deadline
    // health report; the evidence then runs while the rest of boot is settled.
    let mut demo_pending = demo_from_args();
    let mut demo_children = 0u64;
    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let now = sys::clock();
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        // Wake each poll while a demo child is alive, to reap its exit; with
        // nothing to reap, park forever.
        let deadline = if demo_children > 0 {
            now.saturating_add(POLL_TICKS)
        } else {
            0
        };
        match server.recv_with(&mut buffer, (deadline != 0).then_some(deadline)) {
            Ok(message) => {
                let interface = message.interface_id();
                let method = message.method();
                let reply = match dispatch(&mut clipboard, &message) {
                    Ok(reply) => reply,
                    Err(error) => wire::error_reply(interface, method, error),
                };
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race: the kernel expired the transaction and the reply
                    // is `-ENOENT`. Keep serving the other session's offers.
                    if let Err(error) = server.reply(txn, &reply) {
                        if error.errno() != Some(-errno::ENOENT) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
        // Non-blocking reap: an expired deadline returns after the next timer
        // sweep, so a demo child that exited is collected promptly.
        while demo_children > 0 && sys::wait(sys::clock()).is_some() {
            demo_children -= 1;
        }
    }
}

/// The kernel-stamped actor for a message: the service holds `CAP_SETUID`, so
/// it may read another task's credential block.
fn actor(message: &Message) -> messenger::Result<sys::Cred> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).map_err(Error::Errno)?;
    Ok(cred)
}

/// Route one inbound message.
fn dispatch(clipboard: &mut Clipboard, message: &Message) -> messenger::Result<Parcel> {
    match (message.interface_id(), message.method()) {
        (wire::WRITE_INTERFACE, wire::method::OFFER) => {
            let request = wire::decode_offer(&message.parcel)?;
            let cred = actor(message)?;
            let token = clipboard.offer(request, cred.session, message.sender, sys::clock())?;
            wire::token_reply(token)
        }
        (wire::READ_INTERFACE, wire::method::REQUEST) => {
            let (token, mime) = wire::decode_request(&message.parcel)?;
            let cred = actor(message)?;
            let handle = clipboard.request(token, &mime, cred.session);
            match &handle {
                Ok(handle) => clipboard.log_paste(&cred, message.sender, handle),
                Err(Error::Errno(code)) if *code == -errno::EACCES => {
                    clipboard.log_denial(&cred, message.sender, &mime, token);
                }
                Err(_) => {}
            }
            wire::request_reply(&handle?)
        }
        (wire::INTERFACE, wire::method::CURRENT) => {
            let cred = actor(message)?;
            wire::current_reply(clipboard.current(cred.session).as_ref())
        }
        (wire::INTERFACE, wire::method::PING) => {
            Ok(wire::ok_reply(wire::INTERFACE, wire::method::PING))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

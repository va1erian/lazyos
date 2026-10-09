//! The login prompt's keyboard through `inputd` (issue #396, input plan I5).
//!
//! While it prompts, `logind` claims the console (syscall 25 op 7; only it
//! holds `CAP_INPUT_CONSOLE`): the kernel then keeps typed keys off the
//! terminal queue, and `inputd` admits this task's *sessionless* input
//! session, which it feeds while no compositor takes the keyboard. Keys
//! arrive as `TextInput` (characters, layout applied by `inputd`) and
//! `KeyEvent` (Enter, Backspace). Before the console shell starts, the
//! session is closed and the claim released, so the shell reads the
//! terminal as before and never sees the user name or the password.
//!
//! Without `inputd` (or the claim) the prompt falls back to the kernel
//! terminal (`sys::read_char`), as it always did. Serial:
//! `LOGIN:CONSOLE:INPUTD session=<id>` once a session opened,
//! `LOGIN:CONSOLE:KERNEL why=<reason>` on the first fallback.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;

use user::messenger::input::{self as api, wire};
use user::messenger::{create_pair, registry, services, Endpoint, Error, DEFAULT_BUFFER};
use user::sys;

/// Ticks the `Open`/`Close` calls may take.
const CALL_TICKS: u64 = 100;
/// The byte `read_line` treats as end of line, and as rubout.
const ENTER: u8 = b'\n';
const BACKSPACE: u8 = 8;

/// The console session, while one is open.
struct Session {
    input: Endpoint,
    events: Endpoint,
    id: u64,
}

/// Where the prompt reads keys from.
pub(super) struct ConsoleKeys {
    session: Option<Session>,
    pending: VecDeque<u8>,
    buffer: Vec<u8>,
    warned: bool,
}

impl ConsoleKeys {
    pub(super) fn new() -> ConsoleKeys {
        ConsoleKeys {
            session: None,
            pending: VecDeque::new(),
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
            warned: false,
        }
    }

    /// Start reading a prompt: claim the console and open the session, or
    /// fall back to the kernel terminal.
    pub(super) fn begin(&mut self) {
        if self.session.is_some() {
            return;
        }
        match open() {
            Ok(session) => {
                sys::write_str(&format!("LOGIN:CONSOLE:INPUTD session={}\n", session.id));
                self.session = Some(session);
            }
            Err(why) => {
                let _ = sys::input_console_release();
                if !self.warned {
                    self.warned = true;
                    sys::write_str(&format!("LOGIN:CONSOLE:KERNEL why={why}\n"));
                }
            }
        }
    }

    /// Stop reading: close the session and give the keyboard back to the
    /// terminal (before a console shell starts).
    pub(super) fn end(&mut self) {
        self.pending.clear();
        if let Some(session) = self.session.take() {
            let body = wire::encode_close_args(&wire::CloseArgs {
                session: session.id,
            })
            .unwrap_or_default();
            let parcel = api::request(api::INTERFACE, wire::METHOD_CLOSE, body, Vec::new());
            let _ = session.input.call(&parcel, Some(sys::clock() + CALL_TICKS));
            let _ = session.events.close();
            let _ = session.input.release();
        }
        let _ = sys::input_console_release();
    }

    /// The next typed byte: a printable ASCII character, [`ENTER`] or
    /// [`BACKSPACE`]. Blocks until one arrives.
    pub(super) fn read_byte(&mut self) -> u64 {
        loop {
            if let Some(byte) = self.pending.pop_front() {
                return u64::from(byte);
            }
            let Some(session) = self.session.as_ref() else {
                return sys::read_char();
            };
            match session.events.recv_with(&mut self.buffer, None) {
                Ok(message) => self.take(message.method(), &message.parcel.body),
                // `inputd` went away: finish this prompt on the terminal.
                Err(_) => self.end(),
            }
        }
    }

    /// Queue what one event typed.
    fn take(&mut self, method: u32, body: &[u8]) {
        match method {
            wire::METHOD_TEXTINPUT => {
                if let Ok(args) = wire::decode_text_input_args(body) {
                    self.pending
                        .extend(args.utf8.bytes().filter(|byte| (32..127).contains(byte)));
                }
            }
            wire::METHOD_KEYEVENT => {
                let Ok(args) = wire::decode_key_event_args(body) else {
                    return;
                };
                if args.state == wire::KEY_STATE_UP {
                    return;
                }
                match args.sym {
                    inputmap::keysym::ENTER | inputmap::keysym::KP_ENTER => {
                        self.pending.push_back(ENTER)
                    }
                    inputmap::keysym::BACKSPACE => self.pending.push_back(BACKSPACE),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Claim the console and open the sessionless session.
fn open() -> Result<Session, &'static str> {
    sys::input_console_claim().map_err(|_| "claim-refused")?;
    let input = registry::resolve(api::NAME).map_err(|_| "no-inputd")?;
    let (events, peer) = create_pair().map_err(|_| "no-channel")?;
    let (body, objects) = wire::encode_open_args(&wire::OpenArgs {
        surface: None,
        events: peer.handle(),
    })
    .unwrap_or_default();
    let parcel = api::request(api::INTERFACE, wire::METHOD_OPEN, body, objects);
    let mut buffer = [0u8; 256];
    let reply = input
        .call_with(&parcel, &mut buffer, Some(sys::clock() + CALL_TICKS))
        .and_then(|reply| match services::error_field(&reply)? {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(reply),
        });
    let opened = reply
        .ok()
        .and_then(|reply| wire::decode_open_reply(&reply.body).ok());
    match opened {
        Some(reply) => Ok(Session {
            input,
            events,
            id: reply.session,
        }),
        None => {
            // The peer may or may not have moved; closing a stale handle
            // only fails harmlessly.
            let _ = peer.close();
            let _ = events.close();
            let _ = input.release();
            Err("open-refused")
        }
    }
}

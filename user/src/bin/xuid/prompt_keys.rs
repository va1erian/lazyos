//! The trusted prompt's keyboard (docs/accounts-plan.md U2, issue #625):
//! keys from `inputd`, under the active layout, from every keyboard.
//!
//! While the prompt is up the compositor gives `inputd`'s focus to a surface
//! of its own, [`PROMPT_SURFACE`], registered with owner 0 (the compositor
//! itself, `RegisterSurface` in idl/input.midl), and reads that surface's
//! session: `TextInput` (what a key types
//! under the active layout, French AZERTY or any other, composed) fills the
//! fields, `KeyEvent` drives Tab, Enter, Escape, Backspace and the arrows.
//! A USB keyboard reaches it like a PS/2 one, since both feed `inputd`. No
//! client can read those keys: a session is opened only by its surface's
//! owner (`inputmap::Router::open`), and this surface is the compositor's.
//!
//! **Fail closed.** The prompt opens only once `inputd` has *confirmed*,
//! with a two-way `SetFocus`, that the keyboard belongs to that surface, so
//! no client window has it: every one of these calls goes on the shell
//! link's private channel (`inputd/shellchan.rs`), which a client cannot
//! fill, and each is retried until [`CONFIRM_TICKS`] ran out. When that
//! cannot be confirmed the prompt is refused (`EAGAIN`, `elevd` refuses the
//! request and audits it): a one-way focus note that found a full queue, or
//! a fallback to the kernel stream while `inputd` still delivered keys to
//! the window focused before, would have handed the password to the asking
//! app (review of #659, H3).
//!
//! The kernel's own key stream (US scancodes, PS/2) is the source only when
//! `inputd` is not running at all (its name is not registered): then no
//! client gets keys from it either. It is ignored by the prompt while the
//! session is open, so nothing is typed twice; it takes over mid-prompt only
//! if `inputd` dies, which ends every client's session with it.
//!
//! Serial: `XUID:PROMPT:KEYS source=inputd|kernel`, or
//! `XUID:PROMPT:REFUSED reason=<why>`.

use alloc::string::String;

use user::messenger::display::key;
use user::messenger::input::{mods, KeyInput, KeySession, SHELL_NAME};
use user::messenger::{errno, registry, Endpoint, Error};
use user::sys;

use super::compositor::Compositor;

/// The prompt's surface id for `inputd`. Real surfaces count up from 1, so
/// they never reach it.
pub(super) const PROMPT_SURFACE: u64 = u64::MAX - 1;
/// How long `inputd` has to confirm the prompt owns the keyboard (PIT ticks):
/// a healthy one answers within a tick; past this the prompt is refused.
const CONFIRM_TICKS: u64 = 100;
/// Pause between two attempts (ns), so a retry does not spin.
const RETRY_NS: u64 = 2_000_000;

/// Where the prompt's keys come from, once it may open.
pub(super) enum KeySource {
    /// `inputd`, through the prompt surface's session; focus confirmed.
    Inputd,
    /// The kernel's key stream: `inputd` is not running at all.
    Kernel,
}

/// HID usages (keyboard page) the prompt acts on.
mod hid {
    pub const ENTER: u32 = 0x28;
    pub const ESCAPE: u32 = 0x29;
    pub const BACKSPACE: u32 = 0x2A;
    pub const TAB: u32 = 0x2B;
    pub const RIGHT: u32 = 0x4F;
    pub const LEFT: u32 = 0x50;
    pub const KEYPAD_ENTER: u32 = 0x58;
}

impl Compositor {
    /// Take the keyboard for the prompt before it opens: `Err` (the reason)
    /// when no client can be shown to have lost it, and the prompt must be
    /// refused.
    pub(super) fn take_prompt_keys(&mut self) -> Result<KeySource, String> {
        if self.input.link.is_none() && !self.reconnect_input() {
            // No link: safe only when there is no `inputd` to feed a client.
            return match registry::resolve(SHELL_NAME) {
                Err(Error::Errno(code)) if code == -errno::ENOENT => {
                    sys::write_str("XUID:PROMPT:KEYS source=kernel reason=no-inputd\n");
                    Ok(KeySource::Kernel)
                }
                Ok(endpoint) => {
                    let _ = endpoint.release();
                    Err(String::from("inputd-unreachable"))
                }
                Err(error) => Err(alloc::format!("resolve:{:?}", error.errno())),
            };
        }
        match self.confirm_prompt_keys() {
            Ok(session) => {
                self.prompt_keys = Some(session);
                self.input.told_focus = Some(Some(PROMPT_SURFACE));
                sys::write_str("XUID:PROMPT:KEYS source=inputd\n");
                Ok(KeySource::Inputd)
            }
            Err(why) => {
                // Whatever `inputd` did with a call that timed out, the
                // prompt surface is gone again and the real focus is told
                // anew on the next pass.
                if let Some(link) = self.input.link.as_ref() {
                    let _ = link.unregister_surface(PROMPT_SURFACE);
                }
                self.input.told_focus = None;
                Err(why)
            }
        }
    }

    /// Register the prompt surface, open its session and move the focus to
    /// it, each confirmed by `inputd` on the private channel.
    fn confirm_prompt_keys(&self) -> Result<KeySession, String> {
        let Some(link) = self.input.link.as_ref() else {
            return Err(String::from("no-link"));
        };
        let deadline = sys::clock() + CONFIRM_TICKS;
        // Owner 0: the compositor itself (`RegisterSurface`, idl/input.midl).
        retry(deadline, || link.register_surface(PROMPT_SURFACE, 0))
            .map_err(|code| alloc::format!("register:{code:?}"))?;
        let session = retry(deadline, || link.open_keys(PROMPT_SURFACE))
            .map_err(|code| alloc::format!("open:{code:?}"))?;
        match retry(deadline, || link.set_focus(Some(PROMPT_SURFACE))) {
            Ok(()) => Ok(session),
            Err(code) => {
                session.close();
                Err(alloc::format!("focus:{code:?}"))
            }
        }
    }

    /// Give the keyboard back (the prompt closed). The real focus is told
    /// again by the caller's `sync_input_now`.
    pub(super) fn close_prompt_keys(&mut self) {
        if let Some(session) = self.prompt_keys.take() {
            session.close();
        }
        if let Some(link) = self.input.link.as_ref() {
            let _ = link.unregister_surface(PROMPT_SURFACE);
        }
        self.input.told_focus = None;
    }

    /// The prompt session's endpoint, for the main loop to park on.
    pub(super) fn prompt_key_events(&self) -> Option<Endpoint> {
        self.prompt_keys.as_ref().map(KeySession::events_endpoint)
    }

    /// Whether `inputd` feeds the prompt (the kernel stream is then ignored).
    pub(super) fn prompt_keys_from_inputd(&self) -> bool {
        self.prompt_keys.is_some()
    }

    /// Feed the prompt what `inputd` delivered.
    pub(super) fn pump_prompt_keys(&mut self) {
        loop {
            let Some(session) = self.prompt_keys.as_mut() else {
                return;
            };
            match session.poll() {
                Ok(Some(KeyInput::Down { code, mods })) => self.prompt_hid_key(code, mods),
                Ok(Some(KeyInput::Text(text))) => {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        self.prompt_char(c);
                    }
                }
                Ok(None) => return,
                // `inputd` went away: the kernel stream takes over.
                Err(_) => {
                    self.prompt_keys = None;
                    sys::write_str("XUID:PROMPT:KEYS source=kernel\n");
                    return;
                }
            }
        }
    }

    /// A key from `inputd`: the editing and navigation keys (the text comes
    /// separately, as `TextInput`).
    fn prompt_hid_key(&mut self, code: u32, held: u32) {
        let code = match code {
            hid::ENTER | hid::KEYPAD_ENTER => key::ENTER,
            hid::ESCAPE => key::ESCAPE,
            hid::BACKSPACE => key::BACKSPACE,
            hid::TAB => key::TAB,
            hid::LEFT => key::LEFT,
            hid::RIGHT => key::RIGHT,
            _ => return,
        };
        self.mods.shift = held & mods::SHIFT != 0;
        self.prompt_key(code);
    }
}

/// `call` until it succeeds or `deadline` passes; `Err` is the last errno.
/// A dead `inputd` is not retried.
fn retry<T>(deadline: u64, mut call: impl FnMut() -> Result<T, Error>) -> Result<T, Option<i64>> {
    loop {
        match call() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let code = error.errno();
                if code == Some(-errno::EPIPE) || sys::clock() >= deadline {
                    return Err(code);
                }
                sys::sleep_ns(RETRY_NS);
            }
        }
    }
}

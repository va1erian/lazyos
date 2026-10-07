//! The trusted prompt (docs/accounts-plan.md U2, issue #625): `elevd` asks,
//! through `os.lazy.display.prompt.v1`, for an administrator to approve one
//! privileged operation, and the compositor itself asks the person at the
//! screen.
//!
//! Why it can be trusted:
//!
//! * only `elevd`'s kernel-stamped identity may open it ([`Compositor::open_prompt`]);
//! * it is drawn last, over a dimmed screen, above every window, panel and
//!   the shell (`prompt_draw.rs`), and no client can raise anything above it;
//! * while it is up, no client gets a key or a pointer event: before it
//!   opens, `inputd` must confirm the focus moved to the compositor's own
//!   prompt surface (which also ends any keyboard grab), or the prompt is
//!   refused (`prompt_keys.rs`, fail closed); grabs are refused, and every
//!   input record goes to the prompt (`event.rs` hands them over first). Its
//!   keys come from `inputd`, through that surface's session: every
//!   keyboard, under the active layout. Only when `inputd` is not running at
//!   all do they come from the kernel's own key stream, which only the
//!   compositor reads;
//! * no client can read the screen: the display protocol has no capture;
//! * it names the asker from what the kernel stamped on `elevd`'s caller:
//!   its uid and account, and its label, resolved here;
//! * Cancel is the default button, Escape cancels, and it gives up by itself
//!   after [`PROMPT_TICKS`]. The password is never shown or logged.
//!
//! The reply is deferred: [`Compositor::take_prompt_reply`] hands it to the
//! main loop once the person answered.

use alloc::format;
use alloc::string::String;

use libmessenger::Parcel;
use messenger_generated::os_lazy_display_prompt_v1 as wire;
use user::messenger::display::key;
use user::messenger::{errno, services, Message};
use user::sys;

use super::compositor::Compositor;
use super::prompt_draw::{hit, Target};
use super::protocol::{Event, EventKind};

/// How long the prompt waits for an answer (PIT ticks): 90 s.
pub(super) const PROMPT_TICKS: u64 = 9000;
/// Longest name or password typed.
const FIELD_MAX: usize = 64;

/// Where the keyboard goes inside the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    Name,
    Password,
    Cancel,
    Approve,
}

/// An open prompt.
pub(super) struct Prompt {
    txn: u64,
    /// What the operation would change.
    pub(super) summary: String,
    /// Who asks: the app (label) and the account.
    pub(super) asker: String,
    /// A refusal to show (a wrong password before).
    pub(super) error: String,
    pub(super) name: String,
    pub(super) secret: String,
    pub(super) focus: Focus,
    deadline: u64,
    /// Keys the prompt took (evidence that none went to a client).
    keys: u32,
}

/// The standard error reply for a prompt request refused at once.
fn refusal(code: i64, text: &str) -> Parcel {
    let mut body = libmessenger::Encoder::new();
    let _ = body.error(messenger_generated::errors::ERROR_FIELD, code as u32, text);
    Parcel {
        header: services::header(wire::INTERFACE_ID, wire::METHOD_PROMPT),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// The asker, as the prompt names it: the app's kernel label (or that it
/// has none) and the account.
fn asker(label_id: u32, user: &str, uid: u32) -> String {
    let mut name = [0u8; sys::MAX_LABEL_BYTES];
    let app = match label_id {
        0 => String::from("A program without an app label"),
        id => match sys::label_name(id, &mut name) {
            Ok(len) => {
                let label = core::str::from_utf8(&name[..len]).unwrap_or("?");
                match label.strip_prefix("app:") {
                    Some(app) => format!("The app {app}"),
                    None => format!("A program labelled {label}"),
                }
            }
            Err(_) => format!("An app (label #{id})"),
        },
    };
    let user: String = user.chars().filter(|c| !c.is_control()).take(32).collect();
    format!("{app}, run by {user} (uid {uid})")
}

impl Compositor {
    /// Whether `message` is a prompt request.
    pub(super) fn is_prompt_request(message: &Message) -> bool {
        message.interface_id() == wire::INTERFACE_ID && message.method() == wire::METHOD_PROMPT
    }

    /// Open the prompt for `message`. `None`: the reply is deferred until the
    /// person answers; `Some`: the refusal to send now.
    pub(super) fn open_prompt(&mut self, message: &Message) -> Option<Parcel> {
        let caller = message.caller();
        if !elevpolicy::is_elevd(caller.uid, caller.label_id, caller.session) {
            sys::write_str(&format!("XUID:PROMPT:DENIED caller_uid={}\n", caller.uid));
            return Some(refusal(
                errno::EPERM,
                "only elevd may ask for an administrator",
            ));
        }
        let Some(txn) = message.txn else {
            return Some(refusal(errno::EINVAL, "a prompt needs a reply"));
        };
        if self.prompt.is_some() {
            return Some(refusal(errno::EBUSY, "another prompt is open"));
        }
        let Ok(args) = wire::decode_prompt_args(&message.parcel.body) else {
            return Some(refusal(errno::EINVAL, "malformed prompt"));
        };
        let short = |text: &str, max: usize| -> String {
            text.chars().filter(|c| !c.is_control()).take(max).collect()
        };
        // The keyboard first (`prompt_keys.rs`): no prompt opens, and no
        // field takes a key, until `inputd` confirmed no client window has
        // it. Otherwise the request is refused and `elevd` refuses it too.
        if let Err(why) = self.take_prompt_keys() {
            sys::write_str(&format!("XUID:PROMPT:REFUSED reason={why}
"));
            return Some(refusal(
                errno::EAGAIN,
                "the keyboard could not be taken from the apps for the prompt",
            ));
        }
        let name = short(&args.admin, FIELD_MAX);
        let focus = if name.is_empty() {
            Focus::Name
        } else {
            Focus::Password
        };
        // Nothing of a client's stays in the middle of the prompt.
        self.alt_tab = None;
        if self.drag_session.is_some() {
            self.drag_cancel();
        }
        if self.resize.is_some() {
            self.cancel_resize();
        }
        self.drag = None;
        self.grab = None;
        self.prompt = Some(Prompt {
            txn,
            summary: short(&args.summary, 200),
            asker: asker(args.label_id, &args.user, args.uid),
            error: short(&args.error, 120),
            name,
            secret: String::new(),
            focus,
            deadline: sys::clock() + PROMPT_TICKS,
            keys: 0,
        });
        sys::write_str(&format!(
            "XUID:PROMPT:UP uid={} label={}\n",
            args.uid, args.label_id
        ));
        self.sync_input_now();
        self.repaint_full();
        None
    }

    /// The reply of a prompt that was answered, for the main loop to send.
    pub(super) fn take_prompt_reply(&mut self) -> Option<(u64, Parcel)> {
        self.prompt_reply.take()
    }

    /// Give up on an unanswered prompt once its time ran out.
    pub(super) fn tick_prompt(&mut self) {
        let expired = self
            .prompt
            .as_ref()
            .is_some_and(|prompt| sys::clock() >= prompt.deadline);
        if expired {
            self.close_prompt(wire::PROMPT_OUTCOME_TIMED_OUT);
        }
    }

    /// The focus `inputd` must hear: no window's while the prompt is up
    /// (the prompt's own surface, when it reads its keys from `inputd`).
    pub(super) fn input_focus(&self) -> Option<u64> {
        if self.prompt.is_none() {
            self.focused
        } else if self.prompt_keys_from_inputd() {
            Some(super::prompt_keys::PROMPT_SURFACE)
        } else {
            None
        }
    }

    /// An input record while the prompt is up: all of it is the prompt's.
    pub(super) fn prompt_event(&mut self, event: Event) {
        match event.kind {
            EventKind::PointerMove => {
                self.pointer = (event.a as i32, event.b as i32);
                self.move_cursor();
            }
            EventKind::PointerDown => self.prompt_click(),
            EventKind::KeyDown => {
                let code = event.a as u32;
                // `inputd` feeds the prompt (`prompt_keys.rs`): the kernel
                // stream's copy of the same key is not typed twice.
                if !self.track_modifier(code, true) && !self.prompt_keys_from_inputd() {
                    self.prompt_key(code);
                }
            }
            EventKind::KeyUp => {
                self.track_modifier(event.a as u32, false);
            }
            EventKind::PointerUp | EventKind::PointerWheel => {}
        }
    }

    /// Keep the modifier state right while the prompt has the keyboard
    /// (none of the compositor's chords fires); whether `code` was one.
    fn track_modifier(&mut self, code: u32, down: bool) -> bool {
        match code {
            key::SHIFT => self.mods.shift = down,
            key::CTRL => self.mods.ctrl = down,
            key::ALT => self.mods.alt = down,
            key::SUPER => self.mods.super_key = down,
            _ => return false,
        }
        true
    }

    fn prompt_click(&mut self) {
        let dims = (self.screen.width(), self.screen.height());
        let Some(target) = hit(dims, self.pointer) else {
            return;
        };
        match target {
            Target::Name => self.set_prompt_focus(Focus::Name),
            Target::Password => self.set_prompt_focus(Focus::Password),
            Target::Cancel => self.close_prompt(wire::PROMPT_OUTCOME_CANCELLED),
            Target::Approve => self.approve_prompt(),
        }
    }

    fn set_prompt_focus(&mut self, focus: Focus) {
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.focus = focus;
        }
        self.repaint_prompt();
    }

    pub(super) fn prompt_key(&mut self, code: u32) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let shift = self.mods.shift;
        prompt.keys = prompt.keys.saturating_add(1);
        match code {
            key::ESCAPE => return self.close_prompt(wire::PROMPT_OUTCOME_CANCELLED),
            key::TAB => {
                prompt.focus = next_focus(prompt.focus, shift);
            }
            key::ENTER => match prompt.focus {
                Focus::Name => prompt.focus = Focus::Password,
                Focus::Password if !prompt.secret.is_empty() => return self.approve_prompt(),
                Focus::Password => {}
                Focus::Cancel => return self.close_prompt(wire::PROMPT_OUTCOME_CANCELLED),
                Focus::Approve => return self.approve_prompt(),
            },
            key::BACKSPACE => {
                if let Some(field) = field_mut(prompt) {
                    field.pop();
                }
            }
            key::LEFT | key::RIGHT if matches!(prompt.focus, Focus::Cancel | Focus::Approve) => {
                prompt.focus = if code == key::LEFT {
                    Focus::Cancel
                } else {
                    Focus::Approve
                };
            }
            code if (0x20..0x7f).contains(&code) && !self.mods.ctrl && !self.mods.alt => {
                push_char(prompt, code as u8 as char);
            }
            _ => return,
        }
        self.repaint_prompt();
    }

    /// Approve: answer with the name and password typed (an empty name or
    /// password is not an answer yet).
    fn approve_prompt(&mut self) {
        let ready = self
            .prompt
            .as_ref()
            .is_some_and(|prompt| !prompt.name.is_empty() && !prompt.secret.is_empty());
        if ready {
            self.close_prompt(wire::PROMPT_OUTCOME_APPROVED);
        } else if let Some(prompt) = self.prompt.as_mut() {
            prompt.error = String::from("Type an administrator's name and password.");
            prompt.focus = if prompt.name.is_empty() {
                Focus::Name
            } else {
                Focus::Password
            };
            self.repaint_prompt();
        }
    }

    /// A character `inputd` typed (`prompt_keys.rs`, any layout).
    pub(super) fn prompt_char(&mut self, c: char) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        prompt.keys = prompt.keys.saturating_add(1);
        push_char(prompt, c);
        self.repaint_prompt();
    }

    /// Close the prompt with `outcome` and queue the reply.
    fn close_prompt(&mut self, outcome: u32) {
        let Some(mut prompt) = self.prompt.take() else {
            return;
        };
        let approved = outcome == wire::PROMPT_OUTCOME_APPROVED;
        let reply = wire::PromptReply {
            outcome,
            name: if approved {
                core::mem::take(&mut prompt.name)
            } else {
                String::new()
            },
            secret: if approved {
                core::mem::take(&mut prompt.secret)
            } else {
                String::new()
            },
        };
        let body = wire::encode_prompt_reply(&reply).unwrap_or_default();
        let parcel = Parcel {
            header: services::header(wire::INTERFACE_ID, wire::METHOD_PROMPT),
            body,
            ..Parcel::default()
        };
        let word = match outcome {
            wire::PROMPT_OUTCOME_APPROVED => "approved",
            wire::PROMPT_OUTCOME_TIMED_OUT => "timedout",
            _ => "cancelled",
        };
        sys::write_str(&format!(
            "XUID:PROMPT:DONE outcome={word} keys={}\n",
            prompt.keys
        ));
        self.prompt_reply = Some((prompt.txn, parcel));
        self.close_prompt_keys();
        self.sync_input_now();
        self.repaint_full();
    }

    /// Repaint the prompt's panel only (typing does not change the rest).
    fn repaint_prompt(&mut self) {
        let dims = (self.screen.width(), self.screen.height());
        self.repaint(super::prompt_draw::panel(dims));
    }
}

/// The text field the keyboard edits, if a field has the focus.
/// Type `c` into the focused field (at most [`FIELD_MAX`] characters).
fn push_char(prompt: &mut Prompt, c: char) {
    if let Some(field) = field_mut(prompt) {
        if field.chars().count() < FIELD_MAX {
            field.push(c);
        }
    }
}

fn field_mut(prompt: &mut Prompt) -> Option<&mut String> {
    match prompt.focus {
        Focus::Name => Some(&mut prompt.name),
        Focus::Password => Some(&mut prompt.secret),
        Focus::Cancel | Focus::Approve => None,
    }
}

/// Tab order: name, password, Cancel, Approve (Shift+Tab backwards).
fn next_focus(focus: Focus, backwards: bool) -> Focus {
    const ORDER: [Focus; 4] = [Focus::Name, Focus::Password, Focus::Cancel, Focus::Approve];
    let at = ORDER.iter().position(|item| *item == focus).unwrap_or(0);
    let next = if backwards {
        at + ORDER.len() - 1
    } else {
        at + 1
    };
    ORDER[next % ORDER.len()]
}

/// Boot check of the tab order: `XUID:PROMPT:PASS` or `XUID:PROMPT:FAIL`.
pub(super) fn selftest_prompt() -> &'static str {
    let forward = next_focus(Focus::Approve, false) == Focus::Name
        && next_focus(Focus::Name, false) == Focus::Password;
    let backward = next_focus(Focus::Name, true) == Focus::Approve;
    if forward && backward {
        "XUID:PROMPT:PASS\n"
    } else {
        "XUID:PROMPT:FAIL\n"
    }
}

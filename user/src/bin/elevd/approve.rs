//! Asking for an administrator: the trusted prompt and the password check.
//!
//! `xuid` draws the prompt (`os.lazy.display.prompt.v1`) above everything,
//! with the asker's account and kernel label, and answers once the person at
//! the screen approved, cancelled or let it time out. An approval names an
//! account and carries its password: `elevd` accepts it only when
//! `accountsd` says that account is an administrator **and** the password is
//! its. A wrong one shows the prompt again with the reason, up to
//! [`PROMPT_ATTEMPTS`]; every failure counts against the asker and the name
//! typed (`accountdb::ratelimit`), so guessing is slowed whoever is asked.
//! A prompt cancelled or left to time out holds the asker back
//! (`elevpolicy::backoff`), and while it is up the requests that arrive are
//! sorted, never left to pile up (`intake.rs`).

use alloc::string::String;

use accountdb::ratelimit::Key;
use elevpolicy::approvals::Caller;
use elevpolicy::PROMPT_ATTEMPTS;
use libmessenger::Parcel;
use messenger_generated::os_lazy_display_prompt_v1 as prompt;
use user::messenger::{accounts, display, errno, registry, services, Error};
use user::sys;

use super::audit::Entry;
use super::{accounts_endpoint, intake, Refusal, State};

/// How long the prompt may stay up (PIT ticks): `xuid` gives up after 90 s,
/// so this only covers a compositor that stopped answering.
const PROMPT_TICKS: u64 = 12_000;

/// How the request was answered.
pub(crate) enum Verdict {
    /// An administrator (named) approved it.
    Granted(String),
    /// No administrator approved it; the last name typed, if any.
    Refused(String),
    Cancelled,
    TimedOut,
    /// The asker is locked out for now.
    Locked,
    /// No prompt could be shown.
    NoDisplay,
    /// `xuid` could not take the keyboard from the apps safely, so it
    /// showed no prompt (`xuid/prompt_keys.rs`).
    NoKeyboard,
}

impl Verdict {
    /// The audit outcome and the refusal the asker gets.
    pub(crate) fn refusal(&self) -> (&'static str, Refusal) {
        match self {
            Verdict::Granted(_) => ("granted", Refusal::new(0, "")),
            Verdict::Refused(_) => (
                "refused",
                Refusal::new(errno::EACCES, "no administrator approved the change"),
            ),
            Verdict::Cancelled => (
                "cancelled",
                Refusal::new(errno::ECANCELED, "the change was cancelled"),
            ),
            Verdict::TimedOut => (
                "timedout",
                Refusal::new(errno::ETIMEDOUT, "nobody answered the administrator prompt"),
            ),
            Verdict::Locked => (
                "locked",
                Refusal::new(
                    errno::EAGAIN,
                    "too many wrong administrator passwords; wait a moment and try again",
                ),
            ),
            Verdict::NoDisplay => (
                "refused",
                Refusal::new(
                    errno::ENODEV,
                    "there is no screen to ask an administrator on",
                ),
            ),
            Verdict::NoKeyboard => (
                "nokeys",
                Refusal::new(
                    errno::EAGAIN,
                    "the keyboard could not be secured for the administrator prompt; try again",
                ),
            ),
        }
    }
}

/// The person's answer to one prompt.
enum Answer {
    Approved { name: String, secret: String },
    Cancelled,
    TimedOut,
}

/// Show the prompt for `record` until an administrator approves, it is
/// cancelled or times out, or the attempts run out.
pub(crate) fn approve(state: &mut State, caller: Caller, record: &Entry, prefill: &str) -> Verdict {
    if state
        .limiter
        .check(&[Key::Caller(caller.uid)], sys::clock())
        .is_err()
    {
        return Verdict::Locked;
    }
    let mut error = String::new();
    let mut last = String::new();
    for _ in 0..PROMPT_ATTEMPTS {
        let answer = match ask(state, record, caller, prefill, &error) {
            Ok(answer) => answer,
            Err(code) if code == Some(errno::EAGAIN) => return Verdict::NoKeyboard,
            Err(_) => return Verdict::NoDisplay,
        };
        let (name, secret) = match answer {
            Answer::Approved { name, secret } => (name, secret),
            Answer::Cancelled => {
                state.backoff.unanswered(caller, sys::clock());
                return Verdict::Cancelled;
            }
            Answer::TimedOut => {
                state.backoff.unanswered(caller, sys::clock());
                return Verdict::TimedOut;
            }
        };
        let keys = [Key::Caller(caller.uid), Key::Name(name.clone())];
        if state.limiter.check(&keys, sys::clock()).is_err() {
            return Verdict::Locked;
        }
        last = name.clone();
        match check(&name, &secret) {
            Ok(true) => {
                state.limiter.succeeded(&keys);
                return Verdict::Granted(name);
            }
            Ok(false) => {
                state.limiter.failed(&keys, sys::clock());
                error = String::from("That is not an administrator's name and password.");
            }
            Err(()) => {
                state.limiter.failed(&keys, sys::clock());
                return Verdict::Locked;
            }
        }
    }
    Verdict::Refused(last)
}

/// Whether `name` is an administrator and `secret` its password. `Err`:
/// `accountsd`'s own brake holds for that name.
fn check(name: &str, secret: &str) -> Result<bool, ()> {
    let Ok(endpoint) = accounts_endpoint() else {
        return Ok(false);
    };
    let admin = accounts::lookup_name(&endpoint, name)
        .ok()
        .flatten()
        .is_some_and(|user| user.admin);
    match accounts::authenticate(&endpoint, name, secret) {
        Ok(ok) => Ok(admin && ok),
        Err(Error::Errno(code)) if code == -errno::EAGAIN => Err(()),
        Err(_) => Ok(false),
    }
}

/// Show one prompt and wait for the answer, sorting the requests that
/// arrive meanwhile. `Err` when `xuid` cannot show one: no display, or its
/// refusal's code (`EAGAIN`: the keyboard could not be secured).
fn ask(
    state: &mut State,
    record: &Entry,
    caller: Caller,
    prefill: &str,
    error: &str,
) -> Result<Answer, Option<i64>> {
    let display = registry::resolve(display::NAME).map_err(|_| None)?;
    let body = prompt::encode_prompt_args(&prompt::PromptArgs {
        summary: record.summary.clone(),
        uid: caller.uid,
        user: record.user.clone(),
        label_id: caller.label,
        admin: String::from(prefill),
        error: String::from(error),
    })
    .map_err(|_| None)?;
    let request = Parcel {
        header: services::header(prompt::INTERFACE_ID, prompt::METHOD_PROMPT),
        body,
        ..Parcel::default()
    };
    let txn = display
        .begin_call(&request, Some(sys::clock() + PROMPT_TICKS))
        .map_err(|_| None)?;
    let reply = intake::await_prompt(state, &display, txn).map_err(|_| None)?;
    if let Some(code) = refused(&reply) {
        return Err(Some(code));
    }
    let reply = prompt::decode_prompt_reply(&reply.body).map_err(|_| None)?;
    Ok(match reply.outcome {
        prompt::PROMPT_OUTCOME_APPROVED => Answer::Approved {
            name: reply.name,
            secret: reply.secret,
        },
        prompt::PROMPT_OUTCOME_TIMED_OUT => Answer::TimedOut,
        _ => Answer::Cancelled,
    })
}

/// The code of `xuid`'s structured refusal (busy, not allowed, keyboard
/// not secured), if the reply is one.
fn refused(reply: &Parcel) -> Option<i64> {
    match services::error_field(reply) {
        Ok(Some(code)) => Some(code),
        Ok(None) => None,
        Err(_) => Some(errno::EIO),
    }
}

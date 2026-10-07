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

use alloc::string::String;

use accountdb::ratelimit::Key;
use elevpolicy::approvals::Caller;
use elevpolicy::PROMPT_ATTEMPTS;
use libmessenger::{Decoder, Kind, Parcel};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_display_prompt_v1 as prompt;
use user::messenger::{accounts, display, errno, registry, services, Error};
use user::sys;

use super::audit::Entry;
use super::{accounts_endpoint, Refusal, State};

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
        let answer = match ask(record, caller, prefill, &error) {
            Ok(answer) => answer,
            Err(()) => return Verdict::NoDisplay,
        };
        let (name, secret) = match answer {
            Answer::Approved { name, secret } => (name, secret),
            Answer::Cancelled => return Verdict::Cancelled,
            Answer::TimedOut => return Verdict::TimedOut,
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

/// Show one prompt and wait for the answer. `Err` when `xuid` cannot show
/// one (no display, or it refused).
fn ask(record: &Entry, caller: Caller, prefill: &str, error: &str) -> Result<Answer, ()> {
    let display = registry::resolve(display::NAME).map_err(|_| ())?;
    let body = prompt::encode_prompt_args(&prompt::PromptArgs {
        summary: record.summary.clone(),
        uid: caller.uid,
        user: record.user.clone(),
        label_id: caller.label,
        admin: String::from(prefill),
        error: String::from(error),
    })
    .map_err(|_| ())?;
    let request = Parcel {
        header: services::header(prompt::INTERFACE_ID, prompt::METHOD_PROMPT),
        body,
        ..Parcel::default()
    };
    let reply = display
        .call(&request, Some(sys::clock() + PROMPT_TICKS))
        .map_err(|_| ())?;
    if refused(&reply) {
        return Err(());
    }
    let reply = prompt::decode_prompt_reply(&reply.body).map_err(|_| ())?;
    Ok(match reply.outcome {
        prompt::PROMPT_OUTCOME_APPROVED => Answer::Approved {
            name: reply.name,
            secret: reply.secret,
        },
        prompt::PROMPT_OUTCOME_TIMED_OUT => Answer::TimedOut,
        _ => Answer::Cancelled,
    })
}

/// Whether the reply is `xuid`'s structured refusal (busy, not allowed).
fn refused(reply: &Parcel) -> bool {
    let mut decoder = Decoder::new(&reply.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            return true;
        }
    }
    false
}

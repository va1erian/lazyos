//! `elevd` (`/system/bin/elevd`): administrator-approved privileged
//! operations (docs/accounts-plan.md U2, issue #625).
//!
//! It runs as the `_elev` system uid with no capability and serves
//! `os.lazy.elevd.v1`. A task of a login session asks for one operation of
//! the fixed table (`libs/elevpolicy`: installs, system settings, the clock,
//! accounts, the power policy, a service restart); `elevd` checks the
//! arguments, has `xuid` show the trusted prompt (`approve.rs`) naming the
//! asker from its kernel stamp, verifies that the name typed there is an
//! administrator's and the password theirs (through `accountsd`), and then
//! performs the operation itself (`perform.rs`). Nobody is handed root or a
//! capability: the services accept these requests from `elevd`'s identity
//! alone, and `elevd` makes them only for what was approved.
//!
//! Every request is audited (`audit.rs`): a serial line and a
//! `system/events/elevd/request` record that `logd` journals to
//! `/logs/elevd.log`. Wrong passwords lock the asker out for a growing
//! delay (`accountdb::ratelimit`). Every change prompts; only the elevated
//! editor's view (`conf.elevate`, then `conf.list`/`conf.get`) stands, for
//! five minutes, for the same uid, label and session, never for an
//! unlabelled caller, and it ends with the session (`sessions.rs`) or a
//! `Release`.
//!
//! A prompt costs the asker something (review of #659, H4): after one was
//! cancelled or timed out, that caller's requests are refused without a
//! prompt for a growing hold, every caller waits a short pause, and a caller
//! has at most one request in hand at a time (`intake.rs`,
//! `elevpolicy::backoff`, `elevpolicy::queue`). So no program can keep the
//! prompt up until the person at the screen cannot reach Log out.
//!
//! A package install shows the package, not its path, and installs the
//! bytes that were shown (`package.rs`); a service restart is limited to
//! `elevpolicy::RESTARTABLE`. Both are refused before any prompt when they
//! do not qualify.
//!
//! Serial: `ELEVD:UP:PASS`, then one `ELEVD:REQUEST op=<op> uid=<uid>
//! label=<id> session=<id> admin=<name> outcome=<outcome> summary="<text>"`
//! per request (`elevpolicy::audit`: no value can forge a line or a field).

#![no_std]
#![no_main]

extern crate alloc;

#[path = "elevd/approve.rs"]
mod approve;
#[path = "elevd/audit.rs"]
mod audit;
#[path = "elevd/intake.rs"]
mod intake;
#[path = "elevd/package.rs"]
mod package;
#[path = "elevd/perform.rs"]
mod perform;
#[path = "elevd/sessions.rs"]
mod sessions;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;

use accountdb::ratelimit::Limiter;
use elevpolicy::approvals::{Approvals, Caller};
use elevpolicy::backoff::{Backoff, Hold};
use elevpolicy::queue::Queue;
use elevpolicy::{Class, Operation};
use libmessenger::{Encoder, Parcel};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_elevd_v1 as wire;
use user::messenger::{self, accounts, errno, registry, services, Endpoint, Error, Message};
use user::sys;

use approve::Verdict;

/// The service's registered name.
pub(crate) const NAME: &str = "os.lazy.elevd";

/// What `elevd` keeps between requests.
pub(crate) struct State {
    /// The service endpoint, read while a prompt is up too (`intake.rs`).
    pub(crate) server: Endpoint,
    /// Standing view approvals.
    pub(crate) approvals: Approvals,
    /// The wrong-password brake, per asker and per administrator name.
    pub(crate) limiter: Limiter,
    /// The prompt-flood brake: holds after unanswered prompts.
    pub(crate) backoff: Backoff,
    /// Requests waiting behind the one being answered.
    pub(crate) queue: Queue<Message>,
    /// The caller of the request being answered.
    pub(crate) active: Option<Caller>,
    /// `logind`'s session records.
    pub(crate) sessions: sessions::Feed,
    pub(crate) audit: audit::Audit,
    /// The receive buffer `intake.rs` reads into.
    pub(crate) intake_buffer: Vec<u8>,
}

/// A refusal: a positive errno-style code and the text the asker shows.
pub(crate) struct Refusal(pub(crate) i64, pub(crate) String);

impl Refusal {
    pub(crate) fn new(code: i64, text: &str) -> Refusal {
        Refusal(code, text.to_string())
    }

    /// A service's refusal, as it reported it.
    pub(crate) fn of(error: Error) -> Refusal {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EIO);
        Refusal(code, error.message().to_string())
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("elevd: administrator-approved operations (issue #625)\n");
    if let Err(error) = run() {
        sys::write_str("elevd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(NAME, &published, &[wire::INTERFACE_ID], 0)?;
    services::init::notify_ready();
    sys::write_str("ELEVD:UP:PASS\n");
    let mut state = State {
        server,
        approvals: Approvals::new(),
        limiter: Limiter::new(),
        backoff: Backoff::new(),
        queue: Queue::new(),
        active: None,
        sessions: sessions::Feed::new(),
        audit: audit::Audit::new(),
        intake_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
    };
    // One receive buffer for the life of the service (the bump allocator
    // never reclaims a per-call one).
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        // What waited behind the last prompt goes first, in order.
        let message = match state.queue.pop() {
            Some((_, message)) => message,
            None => server.recv_with(&mut buffer, None)?,
        };
        let reply = answer_now(&mut state, &message);
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// The kernel-stamped caller of `message`.
pub(crate) fn caller_of(message: &Message) -> Caller {
    let cred = message.caller();
    Caller {
        uid: cred.uid,
        label: cred.label_id,
        session: cred.session,
    }
}

/// Answer `message`: its reply, or its refusal as the error reply.
pub(crate) fn answer_now(state: &mut State, message: &Message) -> Parcel {
    match answer(state, message) {
        Ok(reply) => reply,
        Err(Refusal(code, text)) => error_reply(message.method(), code, &text),
    }
}

fn answer(state: &mut State, message: &Message) -> Result<Parcel, Refusal> {
    if message.interface_id() != wire::INTERFACE_ID {
        return Err(Refusal::new(errno::EINVAL, "not an elevd request"));
    }
    let caller = caller_of(message);
    match message.method() {
        wire::METHOD_REQUEST => {
            let args = wire::decode_request_args(&message.parcel.body)
                .map_err(|_| Refusal::new(errno::EINVAL, "malformed request"))?;
            state.active = Some(caller);
            let result = request(state, caller, &args.operation, &args.args);
            state.active = None;
            let (detail, values) = result?;
            let body = wire::encode_request_reply(&wire::RequestReply { detail, values })
                .map_err(|_| Refusal::new(errno::EIO, "the reply could not be encoded"))?;
            Ok(parcel(wire::METHOD_REQUEST, body))
        }
        wire::METHOD_RELEASE => {
            state.approvals.release(caller);
            sys::write_str(&alloc::format!(
                "ELEVD:RELEASE uid={} label={} session={}\n",
                caller.uid,
                caller.label,
                caller.session
            ));
            Ok(parcel(wire::METHOD_RELEASE, Vec::new()))
        }
        _ => Err(Refusal::new(errno::EINVAL, "unknown method")),
    }
}

/// Read `logind`'s records and end what the sessions that are over held.
fn follow_sessions(state: &mut State) {
    for session in state.sessions.ended() {
        state.approvals.end_session(session);
        state.backoff.end_session(session);
    }
}

/// Wait out the pause after somebody else's cancelled prompt; refuse a
/// caller whose own prompts went unanswered.
fn may_prompt(state: &mut State, caller: Caller) -> Result<(), Refusal> {
    loop {
        match state.backoff.check(caller, sys::clock()) {
            Ok(()) => return Ok(()),
            Err(hold @ Hold::Caller { .. }) => return Err(intake::held(hold)),
            Err(Hold::Quiet { until }) => intake::pause(state, until),
        }
    }
}

/// One `Request`: check, approve, perform, audit.
fn request(
    state: &mut State,
    caller: Caller,
    operation: &str,
    args: &[String],
) -> Result<(String, Vec<String>), Refusal> {
    let mut record = audit::Entry::new(operation, caller);
    let op = match Operation::parse(operation, args) {
        Ok(op) => op,
        Err(why) => {
            state.audit.log(&record, "invalid");
            return Err(Refusal::new(errno::EINVAL, why));
        }
    };
    record.summary = op.summary();
    // The prompt asks the person at the screen: only a login session's
    // program may ask for it.
    if caller.session == 0 {
        state.audit.log(&record, "refused");
        return Err(Refusal::new(
            errno::EPERM,
            "only a program of a login session may ask for an administrator",
        ));
    }
    // What no administrator may be asked to approve (a guarded service's
    // restart), and a package's facts in place of its path: refused or
    // settled before any prompt.
    if let Err(why) = op.permitted() {
        state.audit.log(&record, "refused");
        return Err(Refusal::new(errno::EPERM, why));
    }
    let package = match package::prepare(&op) {
        Some(Ok(approved)) => {
            record.summary = approved.summary.clone();
            Some(approved)
        }
        Some(Err(refusal)) => {
            state.audit.log(&record, "refused");
            return Err(refusal);
        }
        None => None,
    };
    let asker = accounts_endpoint()
        .ok()
        .and_then(|endpoint| accounts::lookup_uid(&endpoint, caller.uid).ok().flatten());
    record.user = asker.as_ref().map_or_else(
        || alloc::format!("uid {}", caller.uid),
        |user| user.name.clone(),
    );
    follow_sessions(state);
    let now = sys::clock();
    if state.approvals.covers(caller, op.class(), now) {
        record.admin = String::from("(standing approval)");
    } else {
        if let Err(refusal) = may_prompt(state, caller) {
            state.audit.log(&record, "held");
            return Err(refusal);
        }
        let prefill = asker
            .as_ref()
            .filter(|user| user.admin)
            .map(|user| user.name.clone())
            .unwrap_or_default();
        match approve::approve(state, caller, &record, &prefill) {
            Verdict::Granted(admin) => {
                record.admin = admin;
                state.backoff.approved(caller);
                state.approvals.grant(caller, op.class(), sys::clock());
            }
            verdict => {
                let (outcome, refusal) = verdict.refusal();
                if let Verdict::Refused(admin) = &verdict {
                    record.admin = admin.clone();
                }
                state.audit.log(&record, outcome);
                return Err(refusal);
            }
        }
    }
    match perform::perform(&op, package.as_ref()) {
        Ok(result) => {
            state.audit.log(&record, "granted");
            Ok(result)
        }
        Err(refusal) => {
            state.audit.log(&record, "failed");
            // A failed one-shot leaves nothing standing.
            if op.class() == Class::Once {
                state.approvals.release(caller);
            }
            Err(refusal)
        }
    }
}

/// `accountsd`'s endpoint.
pub(crate) fn accounts_endpoint() -> messenger::Result<messenger::Endpoint> {
    registry::resolve(accounts::NAME)
}

/// A reply of `method` carrying `body`.
fn parcel(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: services::header(wire::INTERFACE_ID, method),
        body,
        ..Parcel::default()
    }
}

/// The standard error reply: `code` and the friendly `text`.
pub(crate) fn error_reply(method: u32, code: i64, text: &str) -> Parcel {
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder.
    let _ = body.error(ERROR_FIELD, code as u32, text);
    parcel(method, body.finish())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}

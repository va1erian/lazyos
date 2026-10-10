//! One client connection: the handshake, the request loop and the log
//! stream.
//!
//! A connection is served alone (the accept queue holds the next one), so a
//! stuck client never starves the service of anything but more clients. The
//! loop wakes every [`SLICE_MS`] to look for a request and, while the client
//! follows the log, for new bytes in the ring.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use dbgwire::auth::{self, Lockout, NONCE_LEN, PROTO};
use dbgwire::config::Config;
use dbgwire::json::{self, Object, Value};
use dbgwire::logline;
use dbgwire::methods::{self, Access};
use dbgwire::rpc::{self, code, Id};
use user::messenger::netsock::Addr;
use user::messenger::netstd::{is_timeout, TcpStream};
use user::sys;

use super::audit;
use super::handlers::{self, Failure};

/// Milliseconds one receive waits.
const SLICE_MS: u32 = 200;
/// Milliseconds a client has to authenticate.
const AUTH_WINDOW_MS: u64 = 10_000;
/// Milliseconds of silence that end a session (a following client is never
/// idle: the stream is its traffic).
const IDLE_MS: u64 = 10 * 60 * 1000;
/// Milliseconds one write may wait for room.
const WRITE_MS: u32 = 5_000;
/// Most log lines one notification carries.
const BATCH_LINES: usize = 200;

/// State that outlives a connection.
pub(crate) struct Shared {
    pub lockout: Lockout,
    /// The boot-log read buffer, kept between calls.
    pub scratch: Vec<u8>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            lockout: Lockout::new(),
            scratch: Vec::new(),
        }
    }
}

/// A client that asked to follow the log.
struct Follow {
    ring: handlers::Ring,
    /// Offset in the log of the first byte not yet delivered: the start of
    /// the line being written, so a partial last line is held back and sent
    /// whole once its newline arrives.
    cursor: u64,
}

struct Session<'a> {
    stream: &'a TcpStream,
    peer: Addr,
    authed: bool,
    nonce: [u8; NONCE_LEN],
    follow: Option<Follow>,
    /// Whether this session opened the control tier (`control.begin`).
    control: bool,
    /// The service binary being uploaded.
    upload: Option<dbgwire::control::Upload>,
}

fn now_ms() -> u64 {
    sys::monotonic_ns() / 1_000_000
}

impl Session<'_> {
    fn send(&self, line: &str) -> bool {
        let mut data = String::with_capacity(line.len() + 1);
        data.push_str(line);
        data.push('\n');
        self.stream.write_all(data.as_bytes(), WRITE_MS).is_ok()
    }

    fn reply(&self, id: &Id, result: Result<String, Failure>) -> bool {
        match result {
            Ok(json) => self.send(&rpc::result_line(id, &json)),
            Err((code, message)) => self.send(&rpc::error_line(id, code, &message)),
        }
    }
}

/// Serve `stream` until it ends. `peer` is its remote address.
pub(crate) fn run(stream: &TcpStream, peer: Addr, config: &Config, shared: &mut Shared) {
    if config.peer.is_some_and(|only| only != peer.ip) {
        audit::security(peer, "peer not allowed");
        return;
    }
    let mut nonce = [0u8; NONCE_LEN];
    if sys::random(&mut nonce).is_err() {
        audit::security(peer, "no entropy for a nonce");
        return;
    }
    let mut session = Session {
        stream,
        peer,
        authed: false,
        nonce,
        follow: None,
        control: false,
        upload: None,
    };
    let hello = rpc::notification_line(
        "hello",
        &Object::new()
            .str("server", "dbgd")
            .str("proto", PROTO)
            .str("nonce", &auth::hex(&nonce))
            .str("auth", "hmac-sha256")
            .uint("uptime_ms", now_ms())
            .finish(),
    );
    if !session.send(&hello) {
        return;
    }
    audit::note(peer, "connect", "ok");
    serve(&mut session, config, shared);
    audit::note(peer, "disconnect", "ok");
}

fn serve(session: &mut Session, config: &Config, shared: &mut Shared) {
    let started = now_ms();
    let mut last_request = started;
    let mut inbox: Vec<u8> = Vec::new();
    loop {
        let now = now_ms();
        if !session.authed && now - started > AUTH_WINDOW_MS {
            audit::security(session.peer, "no authentication in time");
            let _ = session.send(&rpc::error_line(
                &Id::Null,
                code::UNAUTHENTICATED,
                "authenticate within 10 seconds",
            ));
            return;
        }
        if session.follow.is_none() && now - last_request > IDLE_MS {
            return;
        }
        match session.stream.read(4096, SLICE_MS) {
            Ok(data) if data.is_empty() => return,
            Ok(data) => {
                inbox.extend_from_slice(&data);
                last_request = now;
            }
            Err(error) if is_timeout(&error) => {}
            Err(_) => return,
        }
        while let Some(end) = inbox.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = inbox.drain(..=end).collect();
            let line = &line[..line.len() - 1];
            if !handle_line(session, line, config, shared) {
                return;
            }
        }
        if inbox.len() > rpc::MAX_LINE {
            let _ = session.send(&rpc::error_line(
                &Id::Null,
                code::INVALID_REQUEST,
                "request line too long",
            ));
            audit::security(session.peer, "oversized request line");
            return;
        }
        if session.authed && session.follow.is_some() && !push_log(session, shared) {
            return;
        }
    }
}

/// Answer one request line; `false` ends the session.
fn handle_line(session: &mut Session, line: &[u8], config: &Config, shared: &mut Shared) -> bool {
    let Ok(text) = core::str::from_utf8(line) else {
        let _ = session.send(&rpc::error_line(
            &Id::Null,
            code::PARSE,
            "request is not UTF-8",
        ));
        return true;
    };
    let text = text.trim_end_matches('\r');
    if text.trim().is_empty() {
        return true;
    }
    let request = match rpc::parse_request(text) {
        Ok(request) => request,
        Err(failure) => {
            audit::note(session.peer, "?", "malformed");
            return session.send(&rpc::error_line(&failure.id, failure.code, failure.message));
        }
    };
    let id = request.id.clone().unwrap_or(Id::Null);
    let Some(method) = methods::lookup(&request.method) else {
        audit::note(session.peer, &request.method, "unknown");
        return session.send(&rpc::error_line(
            &id,
            code::METHOD_NOT_FOUND,
            &format!("no method {}", request.method),
        ));
    };
    if method.access != Access::Open && !session.authed {
        audit::security(session.peer, &format!("{} before auth", method.name));
        return session.send(&rpc::error_line(
            &id,
            code::UNAUTHENTICATED,
            "authenticate first (see the hello notification)",
        ));
    }
    if method.access == Access::Control {
        if let Err(why) = control_open(session, config) {
            audit::security(session.peer, &format!("{}: {why}", method.name));
            return session.send(&rpc::error_line(&id, code::DENIED, why));
        }
    }
    if let Err(message) = methods::validate(method, &request.params) {
        audit::note(session.peer, method.name, "bad params");
        return session.send(&rpc::error_line(&id, code::INVALID_PARAMS, &message));
    }
    if method.name == "auth" {
        return authenticate(session, &id, &request.params, config, shared);
    }
    let result = dispatch(session, method.name, &request.params, config, shared);
    audit::note(
        session.peer,
        method.name,
        if result.is_ok() { "ok" } else { "failed" },
    );
    // A notification (no id) gets no answer, but still ran.
    if request.id.is_none() {
        return true;
    }
    session.reply(&id, result)
}

fn authenticate(
    session: &mut Session,
    id: &Id,
    params: &Value,
    config: &Config,
    shared: &mut Shared,
) -> bool {
    if session.authed {
        return session.reply(id, Ok(Object::new().bool("authenticated", true).finish()));
    }
    if let Err(wait) = shared.lockout.check(now_ms()) {
        audit::security(session.peer, "auth while locked out");
        let _ = session.send(&rpc::error_line(
            id,
            code::LOCKED_OUT,
            &format!("too many failures; retry in {} ms", wait),
        ));
        return false;
    }
    let mac = params.get("mac").and_then(Value::as_str).unwrap_or("");
    if auth::verify_client(&config.key, &session.nonce, mac) {
        shared.lockout.succeeded();
        session.authed = true;
        audit::note(session.peer, "auth", "ok");
        let proof = auth::hex(&auth::server_mac(&config.key, &session.nonce));
        return session.reply(
            id,
            Ok(Object::new()
                .bool("authenticated", true)
                .str("server_mac", &proof)
                .finish()),
        );
    }
    shared.lockout.failed(now_ms());
    audit::security(
        session.peer,
        &format!("bad key (failure {})", shared.lockout.failures()),
    );
    // One try per connection: the next one is a new nonce and a new delay.
    let _ = session.send(&rpc::error_line(
        id,
        code::UNAUTHENTICATED,
        "authentication failed",
    ));
    false
}

/// Why a control method may not run in `session`, if it may not.
fn control_open(session: &Session, config: &Config) -> Result<(), &'static str> {
    if !config.control {
        return Err("control is off on this box (diag.dbg.control=1 in lazyos.cfg)");
    }
    if !session.control {
        return Err("call control.begin {\"confirm\":\"control\"} first");
    }
    Ok(())
}

/// `control.begin`: open the control tier for this session, on purpose.
fn begin_control(
    session: &mut Session,
    params: &Value,
    config: &Config,
) -> Result<String, Failure> {
    if !config.control {
        audit::security(session.peer, "control.begin with control off");
        return Err((
            code::DENIED,
            String::from("control is off on this box (diag.dbg.control=1 in lazyos.cfg)"),
        ));
    }
    if text_param(params, "confirm") != Some(dbgwire::control::CONFIRM) {
        return Err((
            code::INVALID_PARAMS,
            String::from("control.begin needs {\"confirm\":\"control\"}"),
        ));
    }
    session.control = true;
    audit::security(session.peer, "control opened");
    Ok(Object::new().bool("control", true).finish())
}

fn dispatch(
    session: &mut Session,
    name: &str,
    params: &Value,
    config: &Config,
    shared: &mut Shared,
) -> Result<String, Failure> {
    match name {
        "ping" => Ok(Object::new().uint("uptime_ms", now_ms()).finish()),
        "methods" => Ok(handlers::methods_list()),
        "log.tail" => handlers::log_tail(params, &mut shared.scratch),
        "log.sources" => handlers::log_sources(),
        "log.follow" => follow(session, params, shared),
        "log.unfollow" => {
            session.follow = None;
            Ok(Object::new().bool("following", false).finish())
        }
        "tasks.list" => handlers::tasks_list(),
        "sysinfo" => handlers::sysinfo(false),
        "mem.stats" => handlers::sysinfo(true),
        "fabric.stats" => handlers::fabric_stats(),
        "msg.registry" => super::msg::msg_registry(),
        "msg.services" => super::msg::msg_services(),
        "msg.topics" => super::msg::msg_topics(),
        "msg.topic" => super::msg::msg_topic(params),
        "devices.list" => handlers::devices_list(),
        "drivers.list" => handlers::drivers_list(),
        "usb.dump" => handlers::usb_dump(),
        "fs.read" => handlers::fs_read(params),
        "hwreport" => handlers::hwreport(&mut shared.scratch),
        "control.begin" => begin_control(session, params, config),
        "service.restart" => super::control::restart(params),
        "service.upload" => super::control::upload(params, &mut session.upload),
        "service.reload" => super::control::reload(params, &mut session.upload),
        "service.revert" => super::control::revert(params),
        "service.reloads" => super::control::reloads(),
        "app.upload" => super::apps::upload(params, &mut session.upload),
        "app.install" => super::apps::install(params, &mut session.upload),
        "app.relaunch" => super::apps::relaunch(params),
        _ => Err((code::METHOD_NOT_FOUND, format!("no handler for {name}"))),
    }
}

fn text_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

/// `log.follow`: answer with the backlog the client asked for (`lines`,
/// default 0), then stream what is logged afterwards.
fn follow(session: &mut Session, params: &Value, shared: &mut Shared) -> Result<String, Failure> {
    let want = params.get("lines").and_then(Value::as_u64).unwrap_or(0) as usize;
    let ring = match text_param(params, "source") {
        None => handlers::Ring::Kernel,
        Some(name) => handlers::Ring::named(name).ok_or_else(|| {
            (
                code::INVALID_PARAMS,
                String::from("log.follow streams \"kernel\" or \"programs\""),
            )
        })?,
    };
    let log = handlers::read_ring(ring, &mut shared.scratch)?;
    let all = log.lines();
    let from = all.len().saturating_sub(want);
    let backlog = json::array(
        all[from..]
            .iter()
            .map(|(pos, text)| logline::parse(text).to_json(Some(*pos))),
    );
    // A partial last line is delivered by the stream once its newline
    // arrives: start the cursor at that line's start.
    let tail = log
        .text
        .rsplit_once('\n')
        .map_or(log.text.as_str(), |(_, tail)| tail);
    session.follow = Some(Follow {
        ring,
        cursor: log.total - tail.len() as u64,
    });
    Ok(Object::new()
        .bool("following", true)
        .uint("cursor", log.total)
        .raw("lines", &backlog)
        .finish())
}

/// Send what the ring gained since the follower's cursor; `false` when the
/// client is gone.
fn push_log(session: &mut Session, shared: &mut Shared) -> bool {
    let Some(ring) = session.follow.as_ref().map(|f| f.ring) else {
        return true;
    };
    let Ok(log) = handlers::read_ring(ring, &mut shared.scratch) else {
        return true;
    };
    let Some(follow) = session.follow.as_mut() else {
        return true;
    };
    if log.total <= follow.cursor {
        return true;
    }
    // Bytes the ring overwrote before they were read are gone: say so.
    let dropped = log.start().saturating_sub(follow.cursor);
    let from = follow.cursor.max(log.start()) - log.start();
    let fresh = log.text.get(from as usize..).unwrap_or("");
    let at = follow.cursor + dropped;
    let (lines, _partial) = logline::split_complete(fresh);
    let mut pos = at;
    let rows: Vec<(u64, &str)> = lines
        .iter()
        .map(|line| {
            let row = (pos, *line);
            pos += line.len() as u64 + 1;
            row
        })
        .collect();
    let consumed = pos;
    follow.cursor = consumed;
    for chunk in rows.chunks(BATCH_LINES) {
        let array = json::array(
            chunk
                .iter()
                .map(|(p, t)| logline::parse(t).to_json(Some(*p))),
        );
        let params = Object::new()
            .raw("lines", &array)
            .uint("cursor", consumed)
            .uint("dropped", dropped)
            .finish();
        if !session.send(&rpc::notification_line("log", &params)) {
            return false;
        }
    }
    if rows.is_empty() && dropped > 0 {
        let params = Object::new().uint("dropped", dropped).finish();
        return session.send(&rpc::notification_line("log", &params));
    }
    true
}

//! Human-readable formatting of declared topic payloads for the event log.
//!
//! Issue #307 replaced the hand-written `key=value` text payloads with typed
//! TLV ones, so `logd` decodes the topics it knows into the same text shape it
//! used to log, and keeps the raw-bytes fallback for undeclared topics and
//! malformed payloads. Every decoder rejects truncated or garbage bytes with
//! an `Err`, never a panic.

use alloc::format;
use alloc::string::String;
use messenger_generated::{
    os_lazy_clipboard_v1 as clipboard, os_lazy_elevd_v1 as elevd, os_lazy_healthd_v1 as healthd,
    os_lazy_init_v1 as init, os_lazy_logind_v1 as logind, os_lazy_mimed_v1 as mimed,
    os_lazy_pkgd_v1 as pkgd, topics,
};

/// Render one event payload for the log: the declared payload decoded to its
/// fields when the topic is known, else the raw UTF-8 text (the pre-#307
/// format, still used by undeclared topics), else a labelled binary fallback.
pub(super) fn describe(topic: &str, payload: &[u8]) -> String {
    if let Some(text) = declared(topic, payload) {
        return text;
    }
    match core::str::from_utf8(payload) {
        Ok(text) if printable(text) => String::from(text),
        // A declared topic whose payload this build does not decode (a newer
        // producer), or a malformed one: say which type it is instead of
        // dumping bytes. A TLV payload of small fields is often valid UTF-8,
        // so control characters count as binary too: echoed raw they put NULs
        // in the serial log, which turns it into a "binary file" for grep.
        _ => match messenger_generated::declared_topic(topic) {
            Some(decl) => format!("<{} payload, {} byte(s)>", decl.payload, payload.len()),
            None => String::from("<binary>"),
        },
    }
}

/// Text a log line can carry as is: no control character but a tab.
fn printable(text: &str) -> bool {
    !text.chars().any(|c| c.is_control() && c != '\t')
}

/// The declared topics `logd` decodes; `None` leaves the raw fallback.
fn declared(topic: &str, payload: &[u8]) -> Option<String> {
    if topics::matches(healthd::TOPIC_SYSTEM_HEALTH_SUMMARY, topic) {
        let record = healthd::decode_system_health_summary(payload).ok()?;
        return Some(format_health(&record));
    }
    if topics::matches(healthd::TOPIC_SYSTEM_HEALTH, topic) {
        let record = healthd::decode_system_health(payload).ok()?;
        return Some(format_health(&record));
    }
    if topics::matches(init::TOPIC_SYSTEM_EVENTS_SERVICE, topic) {
        let event = init::decode_system_events_service(payload).ok()?;
        return Some(format!(
            "state={} pid={} restarts={} status={} health={} detail={}",
            event.state, event.pid, event.restarts, event.status, event.health, event.detail
        ));
    }
    if topic == logind::TOPIC_SYSTEM_EVENTS_LOGIN_START {
        let event = logind::decode_system_events_login_start(payload).ok()?;
        return Some(format!(
            "user={} uid={} session={} pid={} state={}",
            event.user, event.uid, event.session, event.pid, event.state
        ));
    }
    if topics::matches(logind::TOPIC_SYSTEM_EVENTS_LOGIN_SESSION, topic) {
        let event = logind::decode_system_events_login_session(payload).ok()?;
        return Some(format!(
            "user={} uid={} pid={} state={}",
            event.user, event.uid, event.pid, event.state
        ));
    }
    if topic == logind::TOPIC_SYSTEM_EVENTS_LOGIN_DENIED {
        let event = logind::decode_system_events_login_denied(payload).ok()?;
        return Some(format!("user={} reason={}", event.user, event.reason));
    }
    if topic == logind::TOPIC_SYSTEM_EVENTS_LOGIN_END {
        let event = logind::decode_system_events_login_end(payload).ok()?;
        return Some(format!(
            "user={} uid={} session={} status={}",
            event.user, event.uid, event.session, event.status
        ));
    }
    if topic == clipboard::TOPIC_SYSTEM_EVENTS_CLIPBOARD_PASTE {
        let event = clipboard::decode_system_events_clipboard_paste(payload).ok()?;
        return Some(user::messenger::clipboard::paste_event_text(&event));
    }
    if topic == clipboard::TOPIC_SYSTEM_EVENTS_SECURITY_CLIPBOARD {
        let event = clipboard::decode_system_events_security_clipboard(payload).ok()?;
        return Some(user::messenger::clipboard::denial_event_text(&event));
    }
    if topics::matches(mimed::TOPIC_SYSTEM_EVENTS_OPEN, topic) {
        let event = mimed::decode_system_events_open(payload).ok()?;
        return Some(format!(
            "path={} mime={} verb={}",
            event.path, event.mime, event.verb
        ));
    }
    if topics::matches(pkgd::TOPIC_SYSTEM_EVENTS_PKG, topic) {
        let event = pkgd::decode_system_events_pkg(payload).ok()?;
        return Some(format!(
            "op={} app={} version={} ok={} uid={} detail={}",
            event.op, event.system_name, event.version, event.ok, event.actor_uid, event.detail
        ));
    }
    // `elevd`'s audit trail (docs/accounts-plan.md U2): `/logs/elevd.log`.
    if topics::matches(elevd::TOPIC_SYSTEM_EVENTS_ELEVD_REQUEST, topic) {
        let record = elevd::decode_system_events_elevd_request(payload).ok()?;
        return Some(format!(
            "op={} outcome={} uid={} user={} label={} admin={} summary={}",
            record.operation,
            record.outcome,
            record.uid,
            record.user,
            record.label,
            record.admin,
            record.summary
        ));
    }
    None
}

/// One retained health row as its historic `key=value` line.
fn format_health(record: &healthd::HealthRecord) -> String {
    format!(
        "name={} status={} tick={} detail={}",
        record.name, record.status, record.tick, record.detail
    )
}

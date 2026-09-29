//! Health, session and event-log command output.

use alloc::format;
use user::messenger::{logind, services};
use user::sys;

use super::commands::report;

/// `health`: the retained `system/health/*` rows and the aggregate.
pub(crate) fn print_health() {
    let endpoint = match services::resolve_service(services::HEALTHD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_health(&endpoint) {
        Ok((summary, records)) => {
            sys::write_str(&format!(
                "health: {} ({})\n",
                summary.status, summary.detail
            ));
            for record in &records {
                sys::write_str(&format!(
                    "  {:<10} {:<9} {}\n",
                    record.name, record.status, record.detail
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `sessions`: the `logind` session table (issue #101). Bounded wait: `logind`
/// can be sitting at the console prompt, in which case it answers after the
/// next key and this prints a friendly timeout instead of hanging.
pub(crate) fn print_sessions() {
    let endpoint = match services::resolve_service(logind::NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match logind::fetch_sessions(&endpoint) {
        Ok((_, sessions)) if sessions.is_empty() => {
            sys::write_str("sessions: none yet\n");
        }
        Ok((active, sessions)) => {
            sys::write_str(&format!(
                "sessions: {} active, {} total\n",
                active,
                sessions.len()
            ));
            for session in &sessions {
                sys::write_str(&format!(
                    "  #{:<3} {:<10} uid {:<5} pid {:<3} {:<7} t{}\n",
                    session.id,
                    session.user,
                    session.uid,
                    session.pid,
                    session.state,
                    session.started
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `log [tail [n]]`: the newest records from the structured event log.
pub(crate) fn print_log(count: u64) {
    let endpoint = match services::resolve_service(services::LOGD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_log_tail(&endpoint, count) {
        Ok(records) if records.is_empty() => sys::write_str("log: no records yet\n"),
        Ok(records) => {
            for record in &records {
                sys::write_str(&format!(
                    "  #{:<4} t{:<6} {:<32} {}\n",
                    record.seq, record.tick, record.topic, record.detail
                ));
                sys::write_str(&format!("       hash 0x{:016x}\n", record.hash));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `log verify`: recompute the hash chain over the retained records.
pub(crate) fn verify_log() {
    let endpoint = match services::resolve_service(services::LOGD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_log_verify(&endpoint) {
        Ok((true, count)) => {
            sys::write_str(&format!("log: chain intact over {count} record(s)\n"));
        }
        Ok((false, index)) => {
            sys::write_str(&format!("log: CHAIN BROKEN at record {index}\n"));
        }
        Err(error) => report(error.message()),
    }
}

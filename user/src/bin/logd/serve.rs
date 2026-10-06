//! `os.lazy.logd.v1`: `Tail`/`Count`/`Verify` over the ring, `Sources` and
//! `TailFile` over the journals (uid 0 only: they hold every user's activity).

use alloc::vec::Vec;

use logstore::TailError;
use user::messenger::services::logd::wire;
use user::messenger::{self, errno, services, Error, Message, Parcel};

use crate::log::Log;

/// `TailFile` lines when the caller asks for more.
const MAX_TAIL_LINES: usize = 200;
/// Bytes of lines in one `TailFile` reply: the newest lines that fit, well
/// inside a client's default receive buffer.
const MAX_TAIL_BYTES: usize = messenger::DEFAULT_BUFFER - 2048;

/// The reply to one request: a result, or a structured error.
pub(super) fn reply(log: &mut Log, message: &Message) -> Parcel {
    match dispatch(log, message) {
        Ok(parcel) => parcel,
        Err(error) => services::error_reply(message.interface_id(), message.method(), error),
    }
}

fn dispatch(log: &mut Log, message: &Message) -> messenger::Result<Parcel> {
    if message.interface_id() != services::LOGD_INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    match message.method() {
        wire::METHOD_TAIL => {
            let args = wire::decode_tail_args(&message.parcel.body).map_err(Error::Parcel)?;
            tail(log, args.count.unwrap_or(10) as usize)
        }
        wire::METHOD_COUNT => services::log_count_reply(log.ring.total),
        wire::METHOD_VERIFY => {
            let (ok, index) = log.ring.verify();
            services::log_verify_reply(ok, index)
        }
        wire::METHOD_SOURCES => {
            require_root(message)?;
            let sources = log.journals.sources().map_err(fs_error)?;
            services::log_sources_reply(sources)
        }
        wire::METHOD_TAILFILE => {
            require_root(message)?;
            let args = wire::decode_tail_file_args(&message.parcel.body).map_err(Error::Parcel)?;
            let count = (args.count as usize).min(MAX_TAIL_LINES);
            let lines = log
                .journals
                .tail(&args.source, count, MAX_TAIL_BYTES)
                .map_err(|error| match error {
                    TailError::Invalid => Error::Errno(-errno::EINVAL),
                    TailError::Missing => Error::Errno(-errno::ENOENT),
                    TailError::Fs(code) => fs_error(code),
                })?;
            services::log_tail_file_reply(lines)
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// The newest `count` ring records.
fn tail(log: &Log, count: usize) -> messenger::Result<Parcel> {
    let retained = log.ring.records();
    let start = retained.len().saturating_sub(count);
    let records: Vec<services::LogRecord> = retained[start..]
        .iter()
        .map(|record| services::LogRecord {
            seq: record.seq,
            tick: record.tick,
            topic: record.topic.clone(),
            detail: record.detail.clone(),
            hash: record.hash,
        })
        .collect();
    services::log_records_reply(&records)
}

/// A file errno as a Messenger error.
fn fs_error(code: i64) -> Error {
    Error::Errno(-code)
}

/// Refuse everyone but uid 0, from the kernel-stamped credential (never a
/// uid the caller chose).
fn require_root(message: &Message) -> messenger::Result<()> {
    if message.caller().uid != 0 {
        return Err(Error::Errno(-errno::EACCES));
    }
    Ok(())
}

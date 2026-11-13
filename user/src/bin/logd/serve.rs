//! `os.lazy.logd.v1`: `Tail`/`Count`/`Verify` over the ring, `Sources` and
//! `TailFile` over the journals (uid 0 only: they hold every user's activity).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::server::{Caller, Fault};
use logstore::TailError;
use user::messenger::services::logd::{wire, LogRecord};
use user::messenger::{self, errno, Error, Message, Parcel};

use crate::log::Log;

/// `TailFile` lines when the caller asks for more.
const MAX_TAIL_LINES: usize = 200;
/// Bytes of lines in one `TailFile` reply: the newest lines that fit, well
/// inside a client's default receive buffer.
const MAX_TAIL_BYTES: usize = messenger::DEFAULT_BUFFER - 2048;

/// The reply to one request, through the generated server.
pub(super) fn reply(log: &mut Log, message: &Message) -> Parcel {
    wire::dispatch(&mut Handler(log), message)
}

struct Handler<'a>(&'a mut Log);

impl wire::Server for Handler<'_> {
    fn tail(&mut self, _: &Caller, count: Option<u64>) -> Result<Vec<LogRecord>, Fault> {
        // The newest `count` ring records.
        let count = count.unwrap_or(10) as usize;
        let retained = self.0.ring.records();
        let start = retained.len().saturating_sub(count);
        Ok(retained[start..]
            .iter()
            .map(|record| LogRecord {
                seq: record.seq,
                tick: record.tick,
                topic: record.topic.clone(),
                detail: record.detail.clone(),
                hash: record.hash,
            })
            .collect())
    }

    fn count(&mut self, _: &Caller) -> Result<u64, Fault> {
        Ok(self.0.ring.total)
    }

    fn verify(&mut self, _: &Caller) -> Result<wire::VerifyReply, Fault> {
        let (ok, index) = self.0.ring.verify();
        Ok(wire::VerifyReply { ok, index })
    }

    fn sources(&mut self, cx: &Caller) -> Result<Vec<String>, Fault> {
        require_root(cx)?;
        Ok(self.0.journals.sources().map_err(fs_error)?)
    }

    fn tail_file(&mut self, cx: &Caller, source: String, count: u64) -> Result<Vec<String>, Fault> {
        require_root(cx)?;
        let count = (count as usize).min(MAX_TAIL_LINES);
        let lines = self
            .0
            .journals
            .tail(&source, count, MAX_TAIL_BYTES)
            .map_err(|error| match error {
                TailError::Invalid => Error::Errno(-errno::EINVAL),
                TailError::Missing => Error::Errno(-errno::ENOENT),
                TailError::Fs(code) => fs_error(code),
            })?;
        Ok(lines)
    }
}

/// A file errno as a Messenger error.
fn fs_error(code: i64) -> Error {
    Error::Errno(-code)
}

/// Refuse everyone but uid 0, from the kernel-stamped credential (never a
/// uid the caller chose).
fn require_root(cx: &Caller) -> Result<(), Fault> {
    if cx.uid != 0 {
        return Err(Error::Errno(-errno::EACCES).into());
    }
    Ok(())
}

//! `healthd`'s request handling: the generated `Server` over its health rows.
//!
//! Split out of `healthd.rs` (issue #194).

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::client::Errno;
use libmessenger::server::{Caller, Fault};
use user::central;
use user::messenger::services::health::wire::{ReportReply, Server, StatusReply};

use super::aggregate::{records, report, summary};
use super::HealthRow;

/// The rows and the broker a request reads and publishes through.
pub(crate) struct Handler<'a> {
    pub(crate) rows: &'a mut Vec<HealthRow>,
    pub(crate) broker: &'a mut central::Publisher,
}

impl Server for Handler<'_> {
    /// A heartbeat without a service name is meaningless.
    fn report(
        &mut self,
        _: &Caller,
        name: String,
        status: String,
        detail: String,
    ) -> Result<ReportReply, Fault> {
        if name.is_empty() {
            return Err(Fault::errno(Errno::INVAL));
        }
        report(self.rows, self.broker, &name, &status, &detail);
        Ok(ReportReply {
            summary: summary(self.rows),
            records: records(self.rows),
        })
    }

    fn status(&mut self, _: &Caller) -> Result<StatusReply, Fault> {
        Ok(StatusReply {
            summary: summary(self.rows),
            records: records(self.rows),
        })
    }
}

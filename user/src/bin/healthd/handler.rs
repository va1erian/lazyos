//! `healthd`'s request handling: dispatching the Messenger interfaces it
//! serves and decoding a `Report`.
//!
//! Split out of `healthd.rs` (issue #194).

use alloc::vec::Vec;
use user::messenger::{
    self, router, services,
    services::health::wire::{decode_report_args, ReportArgs},
    Error, Message, Parcel,
};

use super::aggregate::{records, report, summary};
use super::HealthRow;

/// Dispatch one inbound message: heartbeat reports, the broker, or a status
/// query.
pub(crate) fn dispatch(
    rows: &mut Vec<HealthRow>,
    broker: &mut router::TopicBroker,
    message: &Message,
) -> messenger::Result<Parcel> {
    match message.interface_id() {
        router::INTERFACE => broker.handle(message),
        services::HEALTHD_INTERFACE => match message.method() {
            services::health::METHOD_REPORT => {
                let ReportArgs {
                    name,
                    status,
                    detail,
                } = decode_report(message)?;
                report(rows, broker, &name, &status, &detail);
                services::health_reply(&summary(rows), &records(rows))
            }
            services::health::METHOD_STATUS => {
                services::health_reply(&summary(rows), &records(rows))
            }
            _ => Err(Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// Decode a `Report` request body.
///
/// The generated decoder defaults a missing `name` to the empty string, but a
/// heartbeat without a service name is meaningless, so keep the explicit
/// check the old hand-written field scan made.
fn decode_report(message: &Message) -> messenger::Result<ReportArgs> {
    let args = decode_report_args(&message.parcel.body).map_err(Error::Parcel)?;
    if args.name.is_empty() {
        return Err(Error::Errno(-messenger::errno::EINVAL));
    }
    Ok(args)
}

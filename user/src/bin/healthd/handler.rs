//! `healthd`'s request handling: dispatching the Messenger interfaces it
//! serves and decoding the string fields of a `Report`.
//!
//! Split out of `healthd.rs` (issue #194).

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::{self, router, services, Error, Message, Parcel};

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
            services::healthd_method::REPORT => {
                let name = string_field(message, services::field::NAME)?;
                let status = string_field(message, services::field::STATUS)?;
                let detail = string_field(message, services::field::DETAIL)?;
                report(rows, broker, &name, &status, &detail);
                services::health_reply(&summary(rows), &records(rows))
            }
            services::healthd_method::STATUS => {
                services::health_reply(&summary(rows), &records(rows))
            }
            _ => Err(Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// The first string field with the given id in a message body.
fn string_field(message: &Message, id: u16) -> messenger::Result<String> {
    use libmessenger::{Decoder, Kind};
    let mut decoder = Decoder::new(&message.parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-messenger::errno::EINVAL))
}

//! `healthd` (retained health rows), `sysmond` (issue #144's live snapshot),
//! and `logd` (the hash-chained event log), plus the shared `resolve_service`
//! and `Services`-table fetch used by `messengerctl`.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Kind, Parcel};

use crate::messenger::{errno, registry, Endpoint, Error, Result};

use super::{
    all_u64, field, first_u64, for_each_record, header, healthd_method, logd_method,
    sysmond_method, HealthRecord, LogRecord, ServiceStatus, HEALTHD_INTERFACE, LOGD_INTERFACE,
    SYSMOND_INTERFACE,
};

/// `healthd`'s `Status` request.
pub fn health_request() -> Parcel {
    Parcel {
        header: header(HEALTHD_INTERFACE, healthd_method::STATUS),
        ..Parcel::default()
    }
}

/// `healthd`'s `Report` request: publish `health/<name>`.
pub fn health_report_request(name: &str, status: &str, detail: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::NAME, name).map_err(Error::Parcel)?;
    body.string(field::STATUS, status).map_err(Error::Parcel)?;
    body.string(field::DETAIL, detail).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(HEALTHD_INTERFACE, healthd_method::REPORT),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Encode `healthd`'s `Status` reply: a `SUMMARY` record, then one
/// `SERVICE` record per retained health row.
pub fn health_reply(summary: &HealthRecord, records: &[HealthRecord]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.record(field::SUMMARY, &health_record_encoder(summary)?)
        .map_err(Error::Parcel)?;
    for record in records {
        body.record(field::SERVICE, &health_record_encoder(record)?)
            .map_err(Error::Parcel)?;
    }
    Ok(Parcel {
        header: header(HEALTHD_INTERFACE, healthd_method::STATUS),
        body: body.finish(),
        ..Parcel::default()
    })
}

fn health_record_encoder(record: &HealthRecord) -> Result<Encoder> {
    let mut encoder = Encoder::new();
    encoder
        .string(field::NAME, &record.name)
        .map_err(Error::Parcel)?;
    encoder
        .string(field::STATUS, &record.status)
        .map_err(Error::Parcel)?;
    encoder
        .string(field::DETAIL, &record.detail)
        .map_err(Error::Parcel)?;
    encoder
        .u64(field::TICK, record.tick)
        .map_err(Error::Parcel)?;
    Ok(encoder)
}

/// `sysmond`'s `Snapshot` request (issue #144).
pub fn sysinfo_request() -> Parcel {
    Parcel {
        header: header(SYSMOND_INTERFACE, sysmond_method::SNAPSHOT),
        ..Parcel::default()
    }
}

/// Encode `sysmond`'s `Snapshot` reply: the raw fixed-layout `sysinfo`
/// words as one bytes field.
pub fn sysinfo_reply(snapshot: &crate::sysinfo::Snapshot) -> Result<Parcel> {
    let mut wire = [0u8; crate::sysinfo::SIZE];
    if !snapshot.write_bytes(&mut wire) {
        return Err(Error::Errno(-errno::E2BIG));
    }
    let mut body = Encoder::new();
    body.bytes(field::SYSDATA, &wire).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(SYSMOND_INTERFACE, sysmond_method::SNAPSHOT),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// A service's error answer for a request on `interface_id`/`method`: the
/// errno-style code plus friendly text in a structured [`field::ERROR`].
pub fn error_reply(interface_id: u64, method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    Parcel {
        header: header(interface_id, method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// Call `sysmond`'s `Snapshot` and decode the fixed-layout reply; a
/// service failure comes back as its original errno.
pub fn fetch_sysinfo(endpoint: &Endpoint) -> Result<crate::sysinfo::Snapshot> {
    let reply = endpoint.call(&sysinfo_request(), None)?;
    let mut decoder = Decoder::new(&reply.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Bytes && field.id == self::field::SYSDATA {
            return crate::sysinfo::decode_bytes(field.payload).ok_or(Error::Errno(-errno::EINVAL));
        }
        if field.kind == Kind::Error && field.id == self::field::ERROR {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Err(Error::Errno(-(code as i64)));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// `logd`'s `Tail` request.
pub fn log_tail_request(count: u64) -> Parcel {
    let mut body = Encoder::new();
    // The request cannot fail: a fresh encoder has room for one field.
    let _ = body.u64(field::COUNT, count);
    Parcel {
        header: header(LOGD_INTERFACE, logd_method::TAIL),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// `logd`'s `Count` request.
pub fn log_count_request() -> Parcel {
    Parcel {
        header: header(LOGD_INTERFACE, logd_method::COUNT),
        ..Parcel::default()
    }
}

/// `logd`'s `Verify` request.
pub fn log_verify_request() -> Parcel {
    Parcel {
        header: header(LOGD_INTERFACE, logd_method::VERIFY),
        ..Parcel::default()
    }
}

/// Encode `logd`'s `Tail` reply.
pub fn log_records_reply(records: &[LogRecord]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for record in records {
        let mut nested = Encoder::new();
        nested.u64(field::SEQ, record.seq).map_err(Error::Parcel)?;
        nested
            .u64(field::TICK, record.tick)
            .map_err(Error::Parcel)?;
        nested
            .string(field::TOPIC, &record.topic)
            .map_err(Error::Parcel)?;
        nested
            .string(field::DETAIL, &record.detail)
            .map_err(Error::Parcel)?;
        nested
            .u64(field::HASH, record.hash)
            .map_err(Error::Parcel)?;
        body.record(field::RECORD, &nested).map_err(Error::Parcel)?;
    }
    Ok(Parcel {
        header: header(LOGD_INTERFACE, logd_method::TAIL),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Encode `logd`'s `Count` reply.
pub fn log_count_reply(count: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::COUNT, count).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(LOGD_INTERFACE, logd_method::COUNT),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Encode `logd`'s `Verify` reply: `OK` (1/0) and the first bad `INDEX`
/// (the record count when the chain is intact).
pub fn log_verify_reply(ok: bool, index: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::OK, ok as u64).map_err(Error::Parcel)?;
    body.u64(field::INDEX, index).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(LOGD_INTERFACE, logd_method::VERIFY),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Resolve a service's registered name.
pub fn resolve_service(name: &str) -> Result<Endpoint> {
    registry::resolve(name)
}

/// Call `init`'s `Services`.
///
/// Allocates the reply buffer per call; a polling loop should use
/// [`fetch_services_with`] and reuse one buffer.
pub fn fetch_services(endpoint: &Endpoint) -> Result<Vec<ServiceStatus>> {
    let mut buf = alloc::vec![0u8; crate::messenger::DEFAULT_BUFFER];
    fetch_services_with(endpoint, &mut buf)
}

/// [`fetch_services`] with a caller-owned reply buffer.
pub fn fetch_services_with(endpoint: &Endpoint, buf: &mut [u8]) -> Result<Vec<ServiceStatus>> {
    let reply = endpoint.call_with(&super::services_request(), buf, None)?;
    let mut statuses = Vec::new();
    for_each_record(&reply, |mut nested| {
        let mut status = ServiceStatus::default();
        while let Ok(Some(field)) = nested.next() {
            match (field.kind, field.id) {
                (Kind::String, self::field::NAME) => {
                    status.name = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::STATE) => {
                    status.state = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, self::field::PID) => {
                    status.pid = field.as_u64().map_err(Error::Parcel)?
                }
                (Kind::U64, self::field::RESTARTS) => {
                    status.restarts = field.as_u64().map_err(Error::Parcel)?
                }
                (Kind::String, self::field::DEPS) => {
                    status.deps = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::HEALTH) => {
                    status.health = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                _ => {}
            }
        }
        statuses.push(status);
        Ok(())
    })?;
    Ok(statuses)
}

/// Call `healthd`'s `Status`; returns the summary and the retained rows.
pub fn fetch_health(endpoint: &Endpoint) -> Result<(HealthRecord, Vec<HealthRecord>)> {
    let reply = endpoint.call(&health_request(), None)?;
    let mut summary = HealthRecord::default();
    let mut records = Vec::new();
    let mut decoder = Decoder::new(&reply.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind != Kind::Struct {
            continue;
        }
        let mut nested = field.nested(0).map_err(Error::Parcel)?;
        let mut record = HealthRecord::default();
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::String, self::field::NAME) => {
                    record.name = String::from(item.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::STATUS) => {
                    record.status = String::from(item.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::DETAIL) => {
                    record.detail = String::from(item.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, self::field::TICK) => {
                    record.tick = item.as_u64().map_err(Error::Parcel)?
                }
                _ => {}
            }
        }
        match field.id {
            self::field::SUMMARY => summary = record,
            self::field::SERVICE => records.push(record),
            _ => {}
        }
    }
    Ok((summary, records))
}

/// Call `logd`'s `Tail`.
pub fn fetch_log_tail(endpoint: &Endpoint, count: u64) -> Result<Vec<LogRecord>> {
    let reply = endpoint.call(&log_tail_request(count), None)?;
    decode_log_records(&reply)
}

/// Call `logd`'s `Count`.
pub fn fetch_log_count(endpoint: &Endpoint) -> Result<u64> {
    let reply = endpoint.call(&log_count_request(), None)?;
    first_u64(&reply).ok_or(Error::Errno(-errno::EINVAL))
}

/// Call `logd`'s `Verify`; returns `(intact, first bad index)`.
pub fn fetch_log_verify(endpoint: &Endpoint) -> Result<(bool, u64)> {
    let reply = endpoint.call(&log_verify_request(), None)?;
    let ok = first_u64(&reply).unwrap_or(0) != 0;
    let index = all_u64(&reply).nth(1).unwrap_or(0);
    Ok((ok, index))
}

/// Decode a `Tail` reply into records.
pub fn decode_log_records(parcel: &Parcel) -> Result<Vec<LogRecord>> {
    let mut records = Vec::new();
    for_each_record(parcel, |mut nested| {
        let mut record = LogRecord::default();
        while let Ok(Some(field)) = nested.next() {
            match (field.kind, field.id) {
                (Kind::U64, self::field::SEQ) => {
                    record.seq = field.as_u64().map_err(Error::Parcel)?
                }
                (Kind::U64, self::field::TICK) => {
                    record.tick = field.as_u64().map_err(Error::Parcel)?
                }
                (Kind::String, self::field::TOPIC) => {
                    record.topic = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::DETAIL) => {
                    record.detail = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, self::field::HASH) => {
                    record.hash = field.as_u64().map_err(Error::Parcel)?
                }
                _ => {}
            }
        }
        records.push(record);
        Ok(())
    })?;
    Ok(records)
}

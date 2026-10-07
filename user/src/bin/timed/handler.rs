//! Request dispatch for `os.lazy.timed.v1`.

use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::timed::{self as api, wire};
use user::messenger::{errno, services, Error, Message, Parcel, Result};
use user::sys;

use crate::state::{now, State};

/// First second `SetTime` refuses (2200-01-01T00:00:00Z), mirroring the
/// kernel's RTC range so an out-of-range value fails here with a clear code.
const MAX_SET_SECS: i64 = 7_258_118_400;

/// Why a clock or zone change was refused (with `EPERM`): the attack harness
/// (`tools/accounts`) matches this text, so a regression that refuses for
/// another reason cannot pass as the policy holding.
const DENIED: &str = "only an administrator, through elevd, may change the clock or the time zone";

/// Route one inbound message. `Ok(parcel)` is the reply; `Err` becomes the
/// structured error reply the caller sees.
pub(super) fn dispatch(state: &mut State, message: &Message) -> Result<Parcel> {
    if message.interface_id() != api::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let method = message.method();
    if matches!(method, wire::METHOD_SETZONE | wire::METHOD_SETTIME) && !authorized(message) {
        return Ok(services::refusal(
            api::INTERFACE,
            method,
            errno::EPERM,
            DENIED,
        ));
    }
    let body = match method {
        wire::METHOD_NOW => now_reply(state)?,
        wire::METHOD_GETZONE => wire::encode_get_zone_reply(&wire::GetZoneReply {
            name: String::from(state.zone.name),
        })
        .map_err(Error::Parcel)?,
        wire::METHOD_SETZONE => {
            let args = wire::decode_set_zone_args(&message.parcel.body).map_err(Error::Parcel)?;
            set_zone(state, &args.name)?;
            Vec::new()
        }
        wire::METHOD_SETTIME => {
            let args = wire::decode_set_time_args(&message.parcel.body).map_err(Error::Parcel)?;
            set_time(state, args.unix_secs)?;
            Vec::new()
        }
        _ => return Err(Error::Errno(-errno::EINVAL)),
    };
    Ok(api::parcel(method, body))
}

fn now_reply(state: &State) -> Result<Vec<u8>> {
    let (secs, centis) = now();
    let local = state.local(secs);
    wire::encode_now_reply(&wire::NowReply {
        unix_ms: secs * 1000 + i64::from(centis) * 10,
        tz_offset_s: local.offset,
        tz_name: String::from(state.zone.name),
        dst: local.dst,
    })
    .map_err(Error::Parcel)
}

/// Validate against the built-in table, persist, then republish the tick so
/// the retained value never shows a stale zone.
fn set_zone(state: &mut State, name: &str) -> Result<()> {
    let zone = timezone::find(name).ok_or(Error::Errno(-errno::EINVAL))?;
    state.store_zone(zone)?;
    state.next_tick = 0;
    Ok(())
}

/// Whether the requester may change the clock or the zone, both machine
/// settings (docs/accounts-plan.md U2): the check is on its kernel-stamped
/// identity, never on anything in the request body. `CAP_SYS_TIME`, or
/// `elevd`, which asks only once an administrator approved the change on
/// the trusted prompt; anyone else gets `EPERM` and [`DENIED`]. A session's
/// zone change reaches `timed` as `elevd`'s write of the confd key it
/// follows.
fn authorized(message: &Message) -> bool {
    let cred = message.caller();
    cred.caps & sys::CAP_SYS_TIME != 0
        || elevpolicy::is_elevd(cred.uid, cred.label_id, cred.session)
}

/// Step the wall clock (the caller is [`authorized`]).
fn set_time(state: &mut State, unix_secs: i64) -> Result<()> {
    if !(0..MAX_SET_SECS).contains(&unix_secs) {
        return Err(Error::Errno(-errno::EINVAL));
    }
    sys::wall_set(unix_secs as u64).map_err(Error::Errno)?;
    state.next_tick = 0;
    Ok(())
}

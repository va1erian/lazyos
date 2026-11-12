//! The handler of `os.lazy.timed.v1`: the six method bodies of the generated
//! [`Server`]. Framing, decoding and the interface check are `dispatch`'s.

use alloc::string::String;

use libmessenger::client::Errno;
use libmessenger::server::{Caller, Fault};
use user::messenger::timed::wire;
use user::messenger::{confd, errno, Error};
use user::sys;

use crate::ntp::Ntp;
use crate::state::{now, State};

/// First second `SetTime` refuses (2200-01-01T00:00:00Z), mirroring the
/// kernel's RTC range so an out-of-range value fails here with a clear code.
const MAX_SET_SECS: i64 = sntp::policy::MAX_UNIX_SECS;

/// Why a clock or zone change was refused (with `EPERM`): the attack harness
/// (`tools/accounts`) matches this text, so a regression that refuses for
/// another reason cannot pass as the policy holding.
const DENIED: &str = "only an administrator, through elevd, may change the clock or the time zone";

/// The service's state, answering requests.
pub(super) struct Handler<'a> {
    pub(super) state: &'a mut State,
    pub(super) ntp: &'a mut Ntp,
}

impl Handler<'_> {
    /// Refuse a caller that may not change the clock or the zone.
    fn authorize(cx: &Caller) -> Result<(), Fault> {
        if authorized(cx) {
            Ok(())
        } else {
            Err(Fault::refuse(Errno::PERM, DENIED))
        }
    }
}

impl wire::Server for Handler<'_> {
    fn now(&mut self, _: &Caller) -> Result<wire::NowReply, Fault> {
        let (secs, centis) = now();
        let local = self.state.local(secs);
        Ok(wire::NowReply {
            unix_ms: secs * 1000 + i64::from(centis) * 10,
            tz_offset_s: local.offset,
            tz_name: String::from(self.state.zone.name),
            dst: local.dst,
        })
    }

    fn get_zone(&mut self, _: &Caller) -> Result<String, Fault> {
        Ok(String::from(self.state.zone.name))
    }

    fn set_zone(&mut self, cx: &Caller, name: String) -> Result<(), Fault> {
        Self::authorize(cx)?;
        // Validate against the built-in table, persist, then republish the
        // tick so the retained value never shows a stale zone.
        let zone = timezone::find(&name).ok_or(Error::Errno(-errno::EINVAL))?;
        self.state.store_zone(zone).map_err(confd::lower)?;
        self.state.next_tick = 0;
        Ok(())
    }

    fn set_time(&mut self, cx: &Caller, unix_secs: i64) -> Result<(), Fault> {
        Self::authorize(cx)?;
        // Below the floor `timed` would lift the clock again at its next
        // start (`ntp::floor_clock`): refuse it now rather than accept and
        // undo it.
        if !(sntp::CLOCK_FLOOR_UNIX..MAX_SET_SECS).contains(&unix_secs) {
            return Err(Error::Errno(-errno::EINVAL).into());
        }
        sys::wall_set_centis(unix_secs as u64 * 100).map_err(Error::Errno)?;
        self.state.next_tick = 0;
        self.ntp.clock_stepped();
        Ok(())
    }

    fn ntp_status(&mut self, _: &Caller) -> Result<wire::NtpStatusReply, Fault> {
        let (unix, _) = now();
        Ok(self.ntp.status(unix))
    }

    fn sync_now(&mut self, _: &Caller) -> Result<(), Fault> {
        self.ntp
            .sync_now()
            .map_err(|code| Fault::from(Error::Errno(-code)))
    }
}

/// Whether the requester may change the clock or the zone, both machine
/// settings (docs/accounts-plan.md U2): the check is on its kernel-stamped
/// identity, never on anything in the request body. `CAP_SYS_TIME`, or
/// `elevd`, which asks only once an administrator approved the change on
/// the trusted prompt; anyone else gets `EPERM` and [`DENIED`]. A session's
/// zone change reaches `timed` as `elevd`'s write of the confd key it
/// follows.
fn authorized(cx: &Caller) -> bool {
    cx.caps & sys::CAP_SYS_TIME != 0 || elevpolicy::is_elevd(cx.uid, cx.label_id, cx.session)
}

/// Answer one inbound message.
pub(super) fn dispatch(
    state: &mut State,
    ntp: &mut Ntp,
    message: &user::messenger::Message,
) -> libmessenger::Parcel {
    wire::dispatch(&mut Handler { state, ntp }, message)
}

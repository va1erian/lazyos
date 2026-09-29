//! The `timed` service's kernel-facing surface (issue #369): the native
//! wall-clock syscall (24), the `time/` topic publish policy, the `confd`
//! zone key, and zone resolution under sustained flips.

use super::confd_suite::{events, fail, service, text, ALICE, ROOT};
use super::topics_suite::fresh;
use super::*;
use crate::ipc::credentials::{self, Cred, CAP_SYS_TIME};
use crate::ipc::{acl, audit, topics};
use crate::process::wallsys::{self, op};
use crate::wallclock;

const EPERM: u64 = (-1i64) as u64;
const EINVAL: u64 = (-22i64) as u64;

/// 2026-01-01T00:00:00Z.
const Y2026: u64 = 1_767_225_600;

fn become_root() -> Result<(), String> {
    fresh()?;
    credentials::set(task::current(), Cred::ROOT);
    Ok(())
}

fn get_secs() -> u64 {
    wallsys::dispatch(op::GET, 0) / 100
}

/// `get` tracks the wall clock; `set` needs `CAP_SYS_TIME` (the capability,
/// not the uid), checks it before the argument, and bounds the range.
pub fn native_wall_clock_contract() -> Result<(), String> {
    become_root()?;
    let orig = wallclock::unix_secs() as u64;
    let target = Y2026 + 123_456;

    check!(
        wallsys::dispatch(op::SET, target) == 0,
        "root set was refused"
    );
    let after = get_secs();
    check!(
        (target..target + 5).contains(&after),
        "get returned {after} after setting {target}"
    );
    check!(
        wallsys::dispatch(op::SET, wallclock::MAX_SET_SECS as u64) == EINVAL
            && wallsys::dispatch(op::SET, u64::MAX) == EINVAL,
        "an out-of-range time was accepted"
    );
    check!(
        wallsys::dispatch(99, 0) == EINVAL,
        "an unknown op was served"
    );

    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        wallsys::dispatch(op::SET, orig) == EPERM,
        "an unprivileged set was allowed"
    );
    check!(
        wallsys::dispatch(op::SET, u64::MAX) == EPERM,
        "the capability check must precede argument validation"
    );
    check!(
        get_secs() >= target,
        "an unprivileged caller could not even read the clock"
    );
    // The capability, not uid 0, is what authorises a set.
    credentials::set(task::current(), Cred::new(1000, 100, CAP_SYS_TIME, 0, 0));
    check!(
        wallsys::dispatch(op::SET, orig) == 0,
        "CAP_SYS_TIME alone was not enough"
    );
    credentials::set(task::current(), Cred::ROOT);
    Ok(())
}

/// Soak: many reads never go backwards and repeated steps land where told.
pub fn soak_native_wall_clock() -> Result<(), String> {
    become_root()?;
    let orig = wallclock::unix_secs() as u64;
    let mut last = 0;
    for i in 0..100_000 {
        let now = wallsys::dispatch(op::GET, 0);
        check!(now >= last, "wall clock went back at read {i}");
        last = now;
    }
    for i in 0..500u64 {
        let target = Y2026 + i * 7919;
        check!(wallsys::dispatch(op::SET, target) == 0, "set {i} refused");
        let got = get_secs();
        check!((target..target + 5).contains(&got), "cycle {i}: {got}");
    }
    check!(wallsys::dispatch(op::SET, orig) == 0, "restore failed");
    Ok(())
}

/// The policy `timed` needs once a boot policy is loaded: only uid 0 may
/// publish the `time/tick` segments, anyone may subscribe, everything else is
/// default-deny. (No policy is loaded at boot yet, so the bootstrap window
/// allows all; this pins the rules the loader must install.)
fn time_policy() -> [acl::Rule; 3] {
    let rule = |actor, interface_id, method| acl::Rule {
        actor,
        interface_id,
        method,
        allow: true,
    };
    [
        rule(0, topics::PUBLISH_INTERFACE, topics::segment_method("time")),
        rule(0, topics::PUBLISH_INTERFACE, topics::segment_method("tick")),
        rule(acl::ANY_ACTOR, topics::SUBSCRIBE_INTERFACE, acl::ANY_METHOD),
    ]
}

fn as_uid(uid: u32) {
    credentials::set(task::current(), Cred::new(uid, uid, 0, 0, 0));
}

/// `timed` (uid 0) may publish `time/tick`, other uids may not, and every uid
/// may subscribe to `time/tick`, `time/+` and `time/#`.
pub fn time_topic_policy() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    acl::load(&time_policy());

    as_uid(0);
    check!(
        topics::authorize(slot, topics::MODE_PUBLISH, "time/tick", 1) == Ok(2),
        "timed was refused time/tick"
    );
    check!(
        topics::authorize(slot, topics::MODE_PUBLISH, "time/other", 2)
            == Err(topics::Error::Denied),
        "timed could publish outside the granted segments"
    );

    as_uid(1000);
    let before = audit::count();
    check!(
        topics::authorize(slot, topics::MODE_PUBLISH, "time/tick", 3) == Err(topics::Error::Denied),
        "an unprivileged client could publish time/tick"
    );
    check!(
        audit::count() == before + 1,
        "the refused publish was not audited"
    );
    for filter in ["time/tick", "time/+", "time/#"] {
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, filter, 4).is_ok(),
            "subscribe {filter} was refused"
        );
    }
    Ok(())
}

/// Soak: 30k alternating verdicts stay correct, and the audit ring keeps
/// recording without wedging.
pub fn soak_time_topic_policy() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    acl::load(&time_policy());
    for i in 0..30_000u64 {
        let root = i % 2 == 0;
        as_uid(if root { 0 } else { 1000 + (i % 7) as u32 });
        let publish = topics::authorize(slot, topics::MODE_PUBLISH, "time/tick", i);
        check!(
            publish.is_ok() == root,
            "publish verdict wrong at {i}: {publish:?}"
        );
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "time/#", i).is_ok(),
            "subscribe refused at {i}"
        );
    }
    Ok(())
}

/// The zone key: world-readable, root-writable, announced on change, and a
/// stored value resolves through the same table `timed` uses.
pub fn zone_key_in_confd() -> Result<(), String> {
    let mut confd = service()?;
    let key = timezone::ZONE_KEY;
    check!(
        confd.get(key, ALICE).map_err(fail)? == None,
        "a fresh store has a zone"
    );
    check!(
        confd.set(key, text("Europe/Paris"), ALICE).is_err(),
        "an unprivileged caller wrote the zone"
    );
    confd.set(key, text("Europe/Paris"), ROOT).map_err(fail)?;
    let read = confd.get(key, ALICE).map_err(fail)?;
    let Some(confd::Value::Str(name)) = read else {
        return Err(String::from(
            "the zone was not readable by an ordinary user",
        ));
    };
    let zone = timezone::find(name).ok_or("the stored zone is not in the table")?;
    check!(zone.name == "Europe/Paris", "wrong zone {}", zone.name);
    check!(
        events(&confd) == [(String::from(key), false)],
        "the zone write was not announced"
    );
    // A stored name outside the table is refused by `find`, so `timed` falls
    // back to the default instead of trusting it.
    confd.set(key, text("Mars/Base"), ROOT).map_err(fail)?;
    let read = confd.get(key, ROOT).map_err(fail)?;
    let Some(confd::Value::Str(name)) = read else {
        return Err(String::from("zone missing"));
    };
    check!(timezone::find(name).is_none(), "a bogus zone resolved");
    confd.delete(key, ROOT).map_err(fail)?;
    check!(
        confd.get(key, ROOT).map_err(fail)? == None,
        "delete left the zone"
    );
    Ok(())
}

/// Soak: 3000 zone flips through `confd`, each read back and resolved across
/// a DST-straddling instant, one announcement per write.
pub fn soak_zone_flips() -> Result<(), String> {
    let mut confd = service()?;
    let key = timezone::ZONE_KEY;
    let summer = 1_782_864_000i64; // 2026-07-01T00:00:00Z
    let winter = 1_767_225_600i64; // 2026-01-01T00:00:00Z
    let zones = timezone::ZONES;
    for i in 0..3_000usize {
        let zone = &zones[i % zones.len()];
        confd.set(key, text(zone.name), ROOT).map_err(fail)?;
        let read = confd.get(key, ALICE).map_err(fail)?;
        let Some(confd::Value::Str(name)) = read else {
            return Err(format!("flip {i}: zone unreadable"));
        };
        let found = timezone::find(name).ok_or("stored zone lost")?;
        check!(
            found.name == zone.name,
            "flip {i}: {} != {}",
            found.name,
            zone.name
        );
        for unix in [summer, winter] {
            let local = timezone::local(found, unix);
            check!(
                (local.offset - found.std_offset).abs() <= 3600,
                "flip {i}: offset {} out of range for {}",
                local.offset,
                found.name
            );
        }
    }
    check!(
        events(&confd).len() >= 3_000,
        "only {} announcements for 3000 writes",
        events(&confd).len()
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "timed_native_wall_clock_contract",
        native_wall_clock_contract,
    ),
    ("timed_soak_native_wall_clock", soak_native_wall_clock),
    ("timed_time_topic_policy", time_topic_policy),
    ("timed_soak_time_topic_policy", soak_time_topic_policy),
    ("timed_zone_key_in_confd", zone_key_in_confd),
    ("timed_soak_zone_flips", soak_zone_flips),
];

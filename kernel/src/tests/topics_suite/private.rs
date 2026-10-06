//! The per-uid topic namespace (`ipc::topics::private`, issue #407):
//! `user/<uid>/...` is that uid's (and root's), no filter reaches into
//! another uid's, no policy rule overrides it, every refusal is audited, and
//! the syscall gate reports it as `-EACCES`.

use super::*;

const SUB: u32 = topics::MODE_SUBSCRIBE;
const PUB: u32 = topics::MODE_PUBLISH;

fn as_uid(uid: u32) -> usize {
    let slot = task::current();
    credentials::set(slot, Cred::new(uid, uid, 0, 0, 0));
    slot
}

fn verdict(slot: usize, mode: u32, name: &str) -> bool {
    topics::authorize(slot, mode, name, 0).is_ok()
}

/// The access matrix for an ordinary uid and for root.
pub fn private_namespace_rules() -> Result<(), String> {
    fresh()?;
    let slot = as_uid(1000);
    let allowed = [
        (SUB, "user/1000/confd/changed/#"),
        (SUB, "user/1000/confd/changed/ui/mode"),
        (SUB, "user/1000/#"),
        (SUB, "user/1000/+/changed"),
        (PUB, "user/1000/notes"),
        (SUB, "system/confd/changed/#"),
        (SUB, "system/+/up"),
        (PUB, "users/1001/x"),
    ];
    for (mode, name) in allowed {
        check!(verdict(slot, mode, name), "uid 1000 was refused {name}");
    }
    let refused = [
        (SUB, "user/1001/confd/changed/#"),
        (PUB, "user/1001/confd/changed/ui"),
        (SUB, "user/+/confd/changed/#"),
        (SUB, "user/#"),
        (SUB, "user"),
        (PUB, "user"),
        (SUB, "user/01000/#"),
        (SUB, "user/10000/#"),
        (SUB, "user/100/#"),
        (SUB, "user/alice/#"),
        (SUB, "#"),
        (SUB, "+/1000/confd/changed/#"),
        (SUB, "+"),
    ];
    for (mode, name) in refused {
        check!(!verdict(slot, mode, name), "uid 1000 was allowed {name}");
    }
    // Root reads every user's subtree, as confd lets it read every key.
    let root = as_uid(0);
    for name in ["user/1001/#", "user/+/confd/changed/#", "#", "user/#"] {
        check!(verdict(root, SUB, name), "root was refused {name}");
    }
    check!(
        verdict(root, PUB, "user/1001/confd/changed/ui/mode"),
        "root (confd) could not announce a user change"
    );
    // uid 7: a one-digit owner, and the largest uid, spelled out.
    let small = as_uid(7);
    check!(verdict(small, SUB, "user/7/#"), "uid 7 refused its own");
    check!(!verdict(small, SUB, "user/70/#"), "uid 7 reached uid 70");
    let big = as_uid(u32::MAX);
    check!(
        verdict(big, SUB, "user/4294967295/#"),
        "uid u32::MAX refused its own"
    );
    Ok(())
}

/// No loaded rule opens the namespace, and every refusal lands in the audit
/// ring with its own reason and the broker's correlation id.
pub fn private_namespace_overrides_policy_and_audits() -> Result<(), String> {
    fresh()?;
    acl::load(&[acl::Rule {
        actor: acl::ANY_ACTOR,
        interface_id: acl::ANY_INTERFACE,
        method: acl::ANY_METHOD,
        allow: true,
    }]);
    let slot = as_uid(1000);
    let before = audit::count();
    let denied = topics::authorize(slot, SUB, "user/1001/confd/changed/#", 0x407);
    check!(
        denied == Err(topics::Error::Denied),
        "an allow-all policy opened another uid's namespace: {denied:?}"
    );
    check!(audit::count() == before + 1, "the refusal was not audited");
    let event = *audit::recent(1).first().ok_or("no audit event")?;
    check!(
        event.uid == 1000
            && !event.allow
            && event.reason_code == acl::reason::PRIVATE_NAMESPACE
            && event.interface_id == topics::SUBSCRIBE_INTERFACE
            && event.txn_id == 0x407,
        "the audit event is wrong: {event:?}"
    );
    // The rule only narrows: a policy deny still applies inside one's own.
    acl::load(&deny_rule(SUB_INTERFACE, topics::segment_method("confd")));
    check!(
        !verdict(slot, SUB, "user/1000/confd/changed/#"),
        "a policy deny was skipped inside the uid's own namespace"
    );
    check!(
        verdict(slot, SUB, "user/1000/notes/#"),
        "the policy deny spread to a neighbour"
    );
    Ok(())
}

const SUB_INTERFACE: u64 = topics::SUBSCRIBE_INTERFACE;

/// The syscall gate refuses a foreign namespace with `-EACCES`.
pub fn private_namespace_syscall_gate() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        as_uid(1000);
        let own = auth_parcel("user/1000/confd/changed/#", SUB, 1)?;
        let (code, result) = authorize_raw(&own);
        check!(code == 0 && result.value == 5, "own namespace -> {code:#x}");
        let foreign = auth_parcel("user/1001/confd/changed/#", SUB, 2)?;
        let (code, _) = authorize_raw(&foreign);
        check!(code == failed(errno::EACCES), "foreign -> {code:#x}");
        let everything = auth_parcel("#", SUB, 3)?;
        let (code, _) = authorize_raw(&everything);
        check!(code == failed(errno::EACCES), "`#` -> {code:#x}");
        Ok(())
    })
}

/// The reference model the soak checks against, written independently of
/// the kernel's digit comparison.
fn model(uid: u32, name: &str) -> bool {
    if uid == 0 {
        return true;
    }
    let mut parts = name.split('/');
    match parts.next() {
        Some("#") | Some("+") => false,
        Some("user") => parts.next() == Some(format!("{uid}").as_str()),
        _ => true,
    }
}

/// Soak: many uids against owner segments that differ by a digit, a prefix,
/// a leading zero or a wildcard, in both modes; every verdict matches the
/// model and the audit ring counts exactly the refusals.
pub fn private_namespace_soak() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    let mut refusals = 0u64;
    let before = audit::total();
    for uid in (0..400u32).chain([999, 1000, 1001, 65_534, u32::MAX]) {
        credentials::set(slot, Cred::new(uid, uid, 0, 0, 0));
        let owners = [
            format!("{uid}"),
            format!("{}", uid.wrapping_add(1)),
            format!("{}", uid / 10),
            format!("0{uid}"),
            format!("{uid}0"),
            String::from("+"),
        ];
        for owner in &owners {
            for (mode, tail) in [(SUB, "confd/changed/#"), (PUB, "confd/changed/ui")] {
                if owner == "+" && mode == PUB {
                    continue;
                }
                let name = format!("user/{owner}/{tail}");
                let want = model(uid, &name);
                check!(
                    verdict(slot, mode, &name) == want,
                    "uid {uid} on {name}: expected {want}"
                );
                refusals += u64::from(!want);
            }
        }
        for name in ["#", "+/x", "system/#"] {
            let want = model(uid, name);
            check!(verdict(slot, SUB, name) == want, "uid {uid} on {name}");
            refusals += u64::from(!want);
        }
    }
    check!(
        audit::total() == before + refusals,
        "audited {} refusals, expected {refusals}",
        audit::total() - before
    );
    Ok(())
}

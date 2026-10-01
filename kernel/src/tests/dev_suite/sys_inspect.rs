//! The read-only inspection ops `devctl` and the Devices app use
//! (`dev::inspect`, issue #481): the inventory shows each owner's uid and
//! rights, the policy op returns exactly the installed class rules, the denial
//! log lists refused claims to `CAP_AUDIT_READ` holders only, and all three
//! honour the buffer capacity, fault cleanly and refuse labelled apps. The
//! stress case reads all three while drivers claim and release.

use alloc::vec;
use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::class;
use crate::dev::errno::*;
use crate::dev::inspect::{DENIAL_WORDS, INVENTORY_WORDS, NO_OWNER, RULE_WORDS};
use crate::dev::report::reason;
use crate::dev::syscall::*;
use crate::ipc::credentials::{Cred, CAP_AUDIT_READ};
use crate::ipc::handles::rights;

/// Every right a device without an interrupt line can grant.
const FULL: u64 = (rights::DEV_ALL & !rights::DEV_IRQ) as u64;
const FILL: u64 = 0xA5A5_A5A5_A5A5_A5A5;

/// Run `op` into a fresh buffer of `capacity` rows of `words` each.
fn read(op: u64, capacity: usize, words: usize) -> Result<(u64, Vec<u64>), String> {
    let mut buf = vec![FILL; capacity.max(1) * words];
    let got = sys(op, buf.as_mut_ptr() as u64, capacity as u64, 0, 0);
    let total = expect_ok(got, "inspection op")?;
    Ok((total, buf))
}

/// A plain user: no capability at all.
fn user() -> Cred {
    Cred::new(1000, 1000, 0, 0, 1)
}

/// The inventory reflects ownership with the owner's uid and rights; the
/// policy op returns the boot rules or `ENOENT`; the denial log is gated by
/// `CAP_AUDIT_READ` and lists class denials; capacities and pointers are
/// handled like `list`; a labelled app is refused all three.
pub fn sys_inspect_reports_owners_rules_and_denials() -> Result<(), String> {
    let fx = Fixture::new()?;
    let nic = add_device(Spec::nic(None))?;
    let usb = add_device(Spec::nic(None).with_class(0x0C, 0x03))?;
    let total = crate::dev::table().lock().len();

    // Before the boot policy: no rules to show.
    enter(spawn_driver(user())?)?;
    let mut probe = vec![FILL; RULE_WORDS];
    expect_errno(
        sys(OP_POLICY, probe.as_mut_ptr() as u64, 1, 0, 0),
        ENOENT,
        "policy before installation",
    )?;

    crate::dev::policy::install_boot_policy();
    let net_uid = netpolicy::NET_UID;
    enter(spawn_driver(Cred::new(
        net_uid,
        net_uid,
        CAP_DEV_CLAIM,
        0,
        1,
    ))?)?;
    expect_ok(claim_plain(nic), "_net claims the NIC")?;
    expect_errno(claim_plain(usb), EACCES, "_net claiming the controller")?;

    // A plain user reads the inventory and the rules.
    enter(spawn_driver(user())?)?;
    let (count, rows) = read(OP_INVENTORY, total, INVENTORY_WORDS)?;
    check!(
        count == total as u64,
        "inventory reported {count} of {total}"
    );
    let row = |id: DeviceId| &rows[usize::from(id.0) * INVENTORY_WORDS..][..INVENTORY_WORDS];
    let nic_row = row(nic);
    check!(
        nic_row[0] & 0xFFFF == u64::from(nic.0) && nic_row[0] >> 16 & 0xFF == 0x02,
        "nic id/class word {:#x}",
        nic_row[0]
    );
    check!(nic_row[2] == class::NET.interface_id, "nic class id");
    check!(
        nic_row[3] & 0xFFFF_FFFF == u64::from(net_uid) && nic_row[3] >> 32 == FULL,
        "nic owner word {:#x}",
        nic_row[3]
    );
    let usb_row = row(usb);
    check!(
        usb_row[2] == class::USB.interface_id && usb_row[3] == NO_OWNER,
        "controller row {usb_row:?}"
    );

    let rules = crate::dev::policy::boot_rules();
    let (count, words) = read(OP_POLICY, rules.len(), RULE_WORDS)?;
    check!(count == rules.len() as u64, "policy reported {count} rules");
    for (index, rule) in rules.iter().enumerate() {
        let got = &words[index * RULE_WORDS..][..RULE_WORDS];
        check!(
            got[0] == u64::from(rule.actor)
                && got[1] == rule.interface_id
                && got[2] == u64::from(rule.method) | u64::from(rule.allow) << 32,
            "rule {index}: {got:?} vs {rule:?}"
        );
    }

    // A short buffer gets a prefix and the true total.
    let (count, words) = read(OP_POLICY, 1, RULE_WORDS + 1)?;
    check!(count == rules.len() as u64, "short policy read {count}");
    check!(
        words[RULE_WORDS..].iter().all(|w| *w == FILL),
        "policy wrote past its capacity"
    );

    // The denial log: refused to a plain user, shown to an auditor.
    let mut log = vec![FILL; 8 * DENIAL_WORDS];
    expect_errno(
        sys(OP_DENIALS, log.as_mut_ptr() as u64, 8, 0, 0),
        EPERM,
        "denials without CAP_AUDIT_READ",
    )?;
    enter(spawn_driver(Cred::new(1001, 1001, CAP_AUDIT_READ, 0, 1))?)?;
    let (count, log) = read(OP_DENIALS, 8, DENIAL_WORDS)?;
    check!(count >= 1, "the class denial is not in the log");
    check!(
        log[1] & 0xFFFF_FFFF == u64::from(net_uid)
            && log[1] >> 32 == u64::from(reason::CLASS_DENIED)
            && log[2] == class::USB.interface_id
            && log[3] == u64::from(usb.0),
        "newest denial {:?}",
        &log[..DENIAL_WORDS]
    );

    // Hostile pointers fault; a labelled app is refused by its label policy.
    {
        let _strict = Strict::on();
        let mut kernel_buf = vec![0u64; INVENTORY_WORDS];
        expect_errno(
            sys(OP_INVENTORY, kernel_buf.as_mut_ptr() as u64, 1, 0, 0),
            EFAULT,
            "inventory into kernel memory",
        )?;
        expect_errno(
            sys(OP_POLICY, u64::MAX - 8, 4, 0, 0),
            EFAULT,
            "policy at the top of memory",
        )?;
    }
    leave(&fx);
    enter(spawn_driver(Cred::new(1002, 1002, CAP_AUDIT_READ, 77, 1))?)?;
    for op in [OP_INVENTORY, OP_POLICY, OP_DENIALS] {
        let mut buf = vec![FILL; 4];
        expect_errno(
            sys(op, buf.as_mut_ptr() as u64, 1, 0, 0),
            EACCES,
            "a labelled app inspecting devices",
        )?;
    }
    Ok(())
}

/// Many rounds of claim, inspect, refused claim, inspect, release: every read
/// agrees with the table at that moment and nothing grows without bound.
pub fn sys_inspect_stress_during_claims() -> Result<(), String> {
    const ROUNDS: usize = 500;
    let _fx = Fixture::new()?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x01))?;
    crate::dev::policy::install_boot_policy();
    let total = crate::dev::table().lock().len();
    let snd = spawn_driver(Cred::new(
        sndpolicy::SND_UID,
        sndpolicy::SND_UID,
        CAP_DEV_CLAIM,
        0,
        1,
    ))?;
    let thief = spawn_driver(Cred::new(
        usbpolicy::USB_UID,
        usbpolicy::USB_UID,
        CAP_DEV_CLAIM | CAP_AUDIT_READ,
        0,
        1,
    ))?;
    let owner_of = |words: &[u64]| words[usize::from(audio.0) * INVENTORY_WORDS + 3] & 0xFFFF_FFFF;
    for round in 0..ROUNDS {
        enter(snd)?;
        let handle = expect_ok(claim_plain(audio), "_snd claims")?;
        let (_, rows) = read(OP_INVENTORY, total, INVENTORY_WORDS)?;
        check!(
            owner_of(&rows) == u64::from(sndpolicy::SND_UID),
            "round {round}: owner {:#x}",
            owner_of(&rows)
        );
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        enter(thief)?;
        expect_errno(claim_plain(audio), EACCES, "_usb on the sound card")?;
        let (_, rows) = read(OP_INVENTORY, total, INVENTORY_WORDS)?;
        check!(
            owner_of(&rows) == NO_OWNER,
            "round {round}: a refused claim shows an owner"
        );
        let (count, _) = read(OP_DENIALS, 4, DENIAL_WORDS)?;
        check!(
            count as usize <= crate::ipc::audit::AUDIT_CAPACITY,
            "round {round}: the denial log grew past the ring"
        );
        let (rules, _) = read(OP_POLICY, 16, RULE_WORDS)?;
        check!(
            rules as usize == crate::dev::policy::boot_rules().len(),
            "round {round}: {rules} rules"
        );
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_sys_inspect_reports_owners_rules_and_denials",
        sys_inspect_reports_owners_rules_and_denials,
    ),
    (
        "dev_sys_inspect_stress_during_claims",
        sys_inspect_stress_during_claims,
    ),
];

//! The driver class rules a normal boot installs (`dev::policy`, issue #481),
//! exercised through `claim`: each driver uid gets its own class with every
//! right a driver needs, is refused every other driver's class and every
//! class nobody is given, a uid that is not a driver gets nothing, and root
//! keeps its ambient authority. The Messenger uid policy stays in its
//! bootstrap window, so the rest of the system is not default-denied. The
//! stress case cycles claims and releases across every driver uid with the
//! boot policy installed.

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method, Class};
use crate::dev::errno::*;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::ipc::credentials::Cred;
use crate::ipc::handles::rights;

/// Every right a device without an interrupt line can grant.
const FULL: u32 = rights::DEV_ALL & !rights::DEV_IRQ;

/// A device of each class the test exercises, without an interrupt line.
struct Devices {
    net: DeviceId,
    usb: DeviceId,
    audio: DeviceId,
    storage: DeviceId,
    display: DeviceId,
    smbus: DeviceId,
}

impl Devices {
    fn add() -> Result<Devices, String> {
        let of = |class, subclass| add_device(Spec::nic(None).with_class(class, subclass));
        Ok(Devices {
            net: of(0x02, 0x00)?,
            usb: of(0x0C, 0x03)?,
            audio: of(0x04, 0x01)?,
            storage: of(0x01, 0x06)?,
            display: of(0x03, 0x00)?,
            smbus: of(0x0C, 0x05)?,
        })
    }

    fn all(&self) -> [DeviceId; 6] {
        [
            self.net,
            self.usb,
            self.audio,
            self.storage,
            self.display,
            self.smbus,
        ]
    }

    /// `(uid, the device of its own class, that class)` for each driver.
    fn drivers(&self) -> [(u32, DeviceId, &'static Class); 3] {
        [
            (netpolicy::NET_UID, self.net, &class::NET),
            (usbpolicy::USB_UID, self.usb, &class::USB),
            (sndpolicy::SND_UID, self.audio, &class::AUDIO),
        ]
    }
}

fn driver(uid: u32) -> Cred {
    Cred::new(uid, uid, CAP_DEV_CLAIM, 0, 1)
}

/// Claim `device` as the current task and require the full rights back.
fn claim_full(device: DeviceId, what: &str) -> Result<u64, String> {
    let handle = expect_ok(claim_plain(device), what)?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(granted == FULL, "{what}: granted {granted:#x}, want {FULL:#x}");
    Ok(handle)
}

/// The latest claim record for `device`.
fn latest_claim(device: DeviceId) -> Option<audit::AuditEvent> {
    audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| {
            event.method == method::CLAIM && report::device_of(event.txn_id) == Some(device)
        })
}

/// After the boot policy: each driver claims its own class with every right
/// and is refused the others (audited as a class denial); a non-driver uid
/// with the capability is refused everything; root claims anything; and the
/// Messenger uid policy is still empty.
pub fn sys_boot_policy_confines_each_driver_to_its_class() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let devices = Devices::add()?;

    // Before installation nothing here refuses (the test suite's default).
    enter(spawn_driver(driver(DRIVER_UID))?)?;
    let handle = claim_full(devices.smbus, "a claim before the boot policy")?;
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    crate::dev::policy::install_boot_policy();
    check!(
        crate::dev::policy::is_installed(),
        "the boot policy is not installed"
    );
    check!(
        !acl::is_loaded(),
        "the class rules went into the Messenger policy and would default-deny it"
    );

    for (uid, own, own_class) in devices.drivers() {
        enter(spawn_driver(driver(uid))?)?;
        let handle = claim_full(own, own_class.name)?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        for other in devices.all().into_iter().filter(|&device| device != own) {
            expect_errno(
                claim_plain(other),
                EACCES,
                "a driver claiming another class",
            )?;
            let record = latest_claim(other).ok_or("a class denial was not audited")?;
            check!(
                !record.allow && record.uid == uid && record.reason_code == reason::CLASS_DENIED,
                "uid {uid} on device {}: record {record:?}",
                other.0
            );
        }
    }

    // A uid holding the capability but named by no rule gets nothing.
    enter(spawn_driver(driver(DRIVER_UID))?)?;
    for device in devices.all() {
        expect_errno(claim_plain(device), EACCES, "an unlisted uid claiming")?;
    }

    // Root keeps its ambient authority (the harness boots drivers as root).
    enter(spawn_driver(driver(0))?)?;
    for device in devices.all() {
        let handle = claim_full(device, "root claiming")?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    }
    for device in devices.all() {
        check!(
            table_state(device).0.is_none(),
            "device {} ended with an owner",
            device.0
        );
    }
    Ok(())
}

/// The installed rules are exactly the three driver crates' tables.
pub fn sys_boot_policy_is_the_driver_tables() -> Result<(), String> {
    let rules = crate::dev::policy::boot_rules();
    let expected = netpolicy::NET_DRIVER_CLASS_RULES.len()
        + usbpolicy::USB_DRIVER_CLASS_RULES.len()
        + sndpolicy::SND_DRIVER_CLASS_RULES.len();
    check!(
        rules.len() == expected,
        "{} rules, want {expected}",
        rules.len()
    );
    let classes = [
        class::NET.interface_id,
        class::USB.interface_id,
        class::AUDIO.interface_id,
    ];
    for rule in &rules {
        check!(rule.allow, "a boot rule denies: {rule:?}");
        check!(
            classes.contains(&rule.interface_id),
            "a boot rule names another class: {rule:?}"
        );
        check!(
            [method::CLAIM, method::MAP, method::DMA].contains(&rule.method),
            "a boot rule names another method: {rule:?}"
        );
        check!(
            rule.actor != 0 && rule.actor != acl::ANY_ACTOR,
            "a boot rule names root or every uid: {rule:?}"
        );
    }
    Ok(())
}

/// Thousands of rounds with the boot policy installed: every driver claims
/// and releases its own device, then tries every other driver's, which is
/// refused while held and while free. Nothing leaks an owner or a quota unit.
pub fn sys_boot_policy_stress_claim_release() -> Result<(), String> {
    const ROUNDS: usize = 1_000;
    let _fx = Fixture::new()?;
    let devices = Devices::add()?;
    crate::dev::policy::install_boot_policy();
    let drivers = devices.drivers();
    let mut slots = [0; 3];
    for (slot, (uid, _, _)) in slots.iter_mut().zip(drivers) {
        *slot = spawn_driver(driver(uid))?;
    }
    for round in 0..ROUNDS {
        for (index, (_, own, _)) in drivers.into_iter().enumerate() {
            enter(slots[index])?;
            let handle = claim_full(own, "own class")?;
            // Another driver tries it while it is held, then once it is free.
            let thief = slots[(index + 1) % slots.len()];
            enter(thief)?;
            check!(claim_plain(own) < 0, "round {round}: a second owner");
            enter(slots[index])?;
            expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
            enter(thief)?;
            expect_errno(claim_plain(own), EACCES, "a driver on a free foreign device")?;
        }
    }
    for device in devices.all() {
        check!(
            table_state(device).0.is_none(),
            "device {} ended with an owner",
            device.0
        );
    }
    for (uid, _, _) in drivers {
        check!(
            crate::quota::usage(uid, crate::quota::Resource::DeviceClaims) == 0,
            "uid {uid} leaked device-claim quota"
        );
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_sys_boot_policy_confines_each_driver_to_its_class",
        sys_boot_policy_confines_each_driver_to_its_class,
    ),
    (
        "dev_sys_boot_policy_is_the_driver_tables",
        sys_boot_policy_is_the_driver_tables,
    ),
    (
        "dev_sys_boot_policy_stress_claim_release",
        sys_boot_policy_stress_claim_release,
    ),
];

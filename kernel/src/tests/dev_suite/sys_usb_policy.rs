//! The USB HID driver's access rules (`libs/usbpolicy`,
//! `docs/usb-hid-plan.md` U5) loaded into the real ACL and exercised through
//! `claim`: `_usb` may claim, map and DMA a USB host controller and nothing
//! else, and no other uid gets one, root included (it skips capability
//! checks, not class rules). The stress case holds the policy across
//! thousands of claim/release generations interleaved with refused claims,
//! so neither a grant nor a refusal leaks an owner.

use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::syscall::*;
use crate::ipc::acl::Rule;
use crate::ipc::credentials::Cred;
use crate::ipc::handles::rights;
use crate::ipc::topics::{fnv1a32, fnv1a64};

/// `usbpolicy` rules, hashed the way the loader will hash them.
fn compiled() -> Vec<Rule> {
    usbpolicy::USB_DRIVER_CLASS_RULES
        .iter()
        .map(|spec| Rule {
            actor: spec.actor,
            interface_id: fnv1a64(spec.interface),
            method: fnv1a32(spec.method),
            allow: spec.allow,
        })
        .collect()
}

fn usb_cred() -> Cred {
    Cred::new(usbpolicy::USB_UID, usbpolicy::USB_UID, CAP_DEV_CLAIM, 0, 1)
}

/// An xHCI controller (`0C/03/30`) with an interrupt line, so it has every
/// right a device can grant.
fn xhci() -> Result<DeviceId, String> {
    add_device(Spec::nic(Some(LINE_B)).with_class(0x0C, 0x03))
}

/// `_usb` gets every right of a USB controller, is refused every other
/// class, and every other uid is refused the controller.
pub fn sys_usb_driver_policy_is_exactly_the_class_rules() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let controller = xhci()?;
    let nic = add_device(Spec::nic(Some(LINE_A)))?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x03))?;
    let smbus = add_device(Spec::nic(None).with_class(0x0C, 0x05))?;
    check!(
        usbpolicy::USB_DRIVER_CLASS_RULES
            .iter()
            .all(|rule| fnv1a64(rule.interface) == class::USB.interface_id),
        "a rule names an interface other than os.kernel.dev.usb"
    );
    check!(
        compiled().iter().map(|r| r.method).collect::<Vec<_>>()
            == [method::CLAIM, method::MAP, method::DMA],
        "the rules do not name claim, map and dma"
    );
    acl::load(&compiled());

    let slot = spawn_driver(usb_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(controller), "_usb claims the xHCI controller")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == rights::DEV_ALL,
        "_usb was granted {granted:#x}, expected every right the device has"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    for (device, what) in [
        (nic, "a NIC"),
        (audio, "a sound card"),
        (smbus, "an SMBus controller"),
    ] {
        expect_errno(claim_plain(device), EACCES, what)?;
    }

    // Other system uids, a plain driver uid and root: all refused.
    for uid in [901, 902, 903, DRIVER_UID, 0] {
        let other = spawn_driver(Cred::new(uid, uid, CAP_DEV_CLAIM, 0, 1))?;
        enter(other)?;
        expect_errno(
            claim_plain(controller),
            EACCES,
            "another uid claiming the xHCI",
        )?;
    }
    for device in [controller, nic, audio, smbus] {
        check!(
            table_state(device).0.is_none(),
            "a refused claim left an owner"
        );
    }
    Ok(())
}

/// Thousands of claim/release generations by `_usb`, each followed by a
/// refused claim from another uid: every grant is the full set, every
/// refusal is EACCES, and the controller ends with no owner.
pub fn sys_usb_policy_stress_generations() -> Result<(), String> {
    const GENERATIONS: usize = 2_000;
    let _fx = Fixture::new()?;
    let controller = xhci()?;
    acl::load(&compiled());
    let usb = spawn_driver(usb_cred())?;
    let intruder = spawn_driver(Cred::new(DRIVER_UID, DRIVER_UID, CAP_DEV_CLAIM, 0, 1))?;
    for generation in 0..GENERATIONS {
        enter(usb)?;
        let handle = expect_ok(claim_plain(controller), "_usb claims")?;
        let granted = handles::get(handle)
            .map_err(|e| e.message().to_string())?
            .rights;
        check!(
            granted == rights::DEV_ALL,
            "generation {generation}: granted {granted:#x}"
        );
        enter(intruder)?;
        // Held: busy or refused, never granted.
        check!(
            claim_plain(controller) < 0,
            "generation {generation}: a second owner"
        );
        enter(usb)?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        enter(intruder)?;
        expect_errno(
            claim_plain(controller),
            EACCES,
            "the intruder on a free controller",
        )?;
    }
    check!(
        table_state(controller).0.is_none(),
        "the controller ended with an owner"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "sys_usb_driver_policy_is_exactly_the_class_rules",
        sys_usb_driver_policy_is_exactly_the_class_rules,
    ),
    (
        "sys_usb_policy_stress_generations",
        sys_usb_policy_stress_generations,
    ),
];

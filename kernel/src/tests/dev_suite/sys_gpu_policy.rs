//! The Intel GPU service's access rules (`libs/gpupolicy`,
//! `docs/kabylake-gpu-plan.md` stage G1) loaded into the real ACL and
//! exercised through `claim`, with the exact sequence `gpud` runs: `_gpu` may
//! claim, map and DMA a display controller and nothing else, and no other
//! uid gets one, root included (it skips capability checks, not class
//! rules). The claim quiesces the function (the reason `gpud` restores
//! decode and bus mastering at once), and the restoring writes need the
//! `dma` right the rules grant. The stress case repeats the whole sequence
//! for thousands of generations, interleaved with refused claims, so neither
//! a grant nor a refusal leaks an owner.

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

/// What `gpud` is granted on a display controller without an interrupt
/// line: everything the function has except `IRQ` (it claims polled).
const GPU_RIGHTS: u32 = rights::DEV_ALL & !rights::DEV_IRQ;

/// `gpupolicy` rules, hashed the way the loader will hash them.
fn compiled() -> Vec<Rule> {
    gpupolicy::GPU_DRIVER_CLASS_RULES
        .iter()
        .map(|spec| Rule {
            actor: spec.actor,
            interface_id: fnv1a64(spec.interface),
            method: fnv1a32(spec.method),
            allow: spec.allow,
        })
        .collect()
}

fn gpu_cred() -> Cred {
    Cred::new(gpupolicy::GPU_UID, gpupolicy::GPU_UID, CAP_DEV_CLAIM, 0, 1)
}

/// A VGA-compatible display controller (`03/00`) without an interrupt line.
fn display() -> Result<DeviceId, String> {
    add_device(Spec::nic(None).with_class(0x03, 0x00))
}

/// `gpud`'s claim sequence: a polled claim, then I/O and memory decode, then
/// bus mastering (the `dma` right).
fn gpud_claim(device: DeviceId) -> Result<u64, String> {
    let handle = expect_ok(claim_plain(device), "_gpu claims the display controller")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == GPU_RIGHTS,
        "_gpu was granted {granted:#x}, expected {GPU_RIGHTS:#x}"
    );
    expect_ok(
        sys(OP_CFG_WRITE, handle, 0x04, 2, 0x0003),
        "restoring I/O and memory decode",
    )?;
    expect_ok(
        sys(OP_CFG_WRITE, handle, 0x04, 2, 0x0007),
        "restoring bus mastering needs the dma right _gpu holds",
    )?;
    Ok(handle)
}

/// `_gpu` gets every right of a display controller but the interrupt it did
/// not ask for, is refused every other class, and every other uid (root
/// included) is refused the display controller.
pub fn sys_gpu_driver_policy_is_exactly_the_class_rules() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let controller = display()?;
    let nic = add_device(Spec::nic(Some(LINE_A)))?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x03))?;
    let usb = add_device(Spec::nic(None).with_class(0x0C, 0x03))?;
    check!(
        gpupolicy::GPU_DRIVER_CLASS_RULES
            .iter()
            .all(|rule| fnv1a64(rule.interface) == class::DISPLAY.interface_id),
        "a rule names an interface other than os.kernel.dev.display"
    );
    check!(
        compiled().iter().map(|r| r.method).collect::<Vec<_>>()
            == [method::CLAIM, method::MAP, method::DMA],
        "the rules do not name claim, map and dma"
    );
    acl::load(&compiled());

    enter(spawn_driver(gpu_cred())?)?;
    let handle = gpud_claim(controller)?;
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    for (device, what) in [
        (nic, "a NIC"),
        (audio, "a sound card"),
        (usb, "a USB controller"),
    ] {
        expect_errno(claim_plain(device), EACCES, what)?;
    }

    // Other system uids, a plain driver uid and root: all refused.
    for uid in [901, 902, 903, 904, 911, 914, DRIVER_UID, 0] {
        let other = spawn_driver(Cred::new(uid, uid, CAP_DEV_CLAIM, 0, 1))?;
        enter(other)?;
        expect_errno(
            claim_plain(controller),
            EACCES,
            "another uid claiming the display controller",
        )?;
    }
    for device in [controller, nic, audio, usb] {
        check!(
            table_state(device).0.is_none(),
            "a refused claim left an owner"
        );
    }
    Ok(())
}

/// Thousands of `gpud` generations (claim, restore decode and bus mastering,
/// release), each followed by a refused claim from another uid: every grant
/// is the same set, every refusal is EACCES, and the controller ends with no
/// owner and no leaked claim quota.
pub fn sys_gpu_policy_stress_generations() -> Result<(), String> {
    const GENERATIONS: usize = 2_000;
    let _fx = Fixture::new()?;
    let controller = display()?;
    acl::load(&compiled());
    let gpu = spawn_driver(gpu_cred())?;
    let intruder = spawn_driver(Cred::new(DRIVER_UID, DRIVER_UID, CAP_DEV_CLAIM, 0, 1))?;
    for generation in 0..GENERATIONS {
        enter(gpu)?;
        let handle = gpud_claim(controller)
            .map_err(|why| alloc::format!("generation {generation}: {why}"))?;
        enter(intruder)?;
        // Held: busy or refused, never granted.
        check!(
            claim_plain(controller) < 0,
            "generation {generation}: a second owner"
        );
        enter(gpu)?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        enter(intruder)?;
        expect_errno(
            claim_plain(controller),
            EACCES,
            "the intruder on a free display controller",
        )?;
    }
    check!(
        table_state(controller).0.is_none(),
        "the controller ended with an owner"
    );
    check!(
        crate::quota::usage(gpupolicy::GPU_UID, crate::quota::Resource::DeviceClaims) == 0,
        "_gpu leaked device-claim quota"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "sys_gpu_driver_policy_is_exactly_the_class_rules",
        sys_gpu_driver_policy_is_exactly_the_class_rules,
    ),
    (
        "sys_gpu_policy_stress_generations",
        sys_gpu_policy_stress_generations,
    ),
];

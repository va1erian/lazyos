//! The networking stack's class rules (`libs/netpolicy`, networking plan N1)
//! loaded into the real ACL and exercised through `claim`.
//!
//! The rules live in a host-tested data crate because nothing loads a policy at
//! boot yet; this test proves the table does what its doc says when something
//! does: `_net` may claim, map and DMA a net-class device and nothing else, and
//! no other uid gets the net class.

use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::syscall::*;
use crate::ipc::acl::{Rule, ANY_METHOD};
use crate::ipc::credentials::Cred;
use crate::ipc::handles::rights;
use crate::ipc::topics::{fnv1a32, fnv1a64};

/// A driver task with `cred`, entered.
fn driver_in(cred: Cred) -> Result<usize, String> {
    let slot = spawn_driver(cred)?;
    enter(slot)?;
    Ok(slot)
}

/// `netpolicy`'s rules, hashed the way the loader will hash them.
fn compiled() -> Vec<Rule> {
    netpolicy::NET_DRIVER_CLASS_RULES
        .iter()
        .map(|spec| Rule {
            actor: spec.actor,
            interface_id: fnv1a64(spec.interface),
            method: if spec.method == netpolicy::ANY_METHOD {
                ANY_METHOD
            } else {
                fnv1a32(spec.method)
            },
            allow: spec.allow,
        })
        .collect()
}

/// `_net` gets the full rights of a NIC on a net-class device, is refused every
/// other class, and another uid holding the same capability is refused the
/// net class.
pub fn sys_net_driver_policy_is_exactly_the_class_rules() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let nic = add_device(Spec::nic(Some(LINE_A)))?;
    let storage = add_device(Spec::nic(None).with_class(0x01, 0))?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x01))?;
    check!(
        netpolicy::NET_DRIVER_CLASS_RULES
            .iter()
            .all(|rule| fnv1a64(rule.interface) == class::NET.interface_id),
        "a rule names an interface other than os.kernel.dev.net"
    );
    check!(
        compiled().iter().map(|r| r.method).collect::<Vec<_>>()
            == [method::CLAIM, method::MAP, method::DMA],
        "the rules do not name claim, map and dma"
    );
    acl::load(&compiled());

    // `_net`: the net class with every right the device has.
    let net_cred = Cred::new(netpolicy::NET_UID, netpolicy::NET_UID, CAP_DEV_CLAIM, 0, 1);
    driver_in(net_cred)?;
    let handle = expect_ok(claim_plain(nic), "_net claims a net-class device")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == rights::DEV_ALL,
        "_net was granted {granted:#x}, expected MMIO, PIO, DMA, IRQ and CONFIG"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    // ... and nothing else.
    expect_errno(
        claim_plain(storage),
        EACCES,
        "_net claiming a storage device",
    )?;
    expect_errno(claim_plain(audio), EACCES, "_net claiming an audio device")?;
    check!(
        table_state(storage).0.is_none() && table_state(audio).0.is_none(),
        "a refused claim left an owner"
    );

    // Another uid with the capability but without the rule.
    // Root is among them: it skips capability checks, not the class rules.
    for uid in [netpolicy::NET_UID + 1, DRIVER_UID, 0] {
        driver_in(Cred::new(uid, uid, CAP_DEV_CLAIM, 0, 1))?;
        expect_errno(
            claim_plain(nic),
            EACCES,
            "a uid without the net-class rule claiming a NIC",
        )?;
    }
    check!(
        table_state(nic).0.is_none(),
        "a refused claim left an owner"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[(
    "dev_sys_net_driver_policy_is_exactly_the_class_rules",
    sys_net_driver_policy_is_exactly_the_class_rules,
)];

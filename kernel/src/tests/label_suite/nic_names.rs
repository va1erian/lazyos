//! The NIC namespace (`os.lazy.net.nic/*`) belongs to the NIC drivers.
//!
//! `netd` hands whoever holds a name there its frame rings and believes the
//! card it describes, so the registry refuses every other identity the name,
//! whatever its uid rules or capabilities say (`netpolicy`). The tests drive
//! the real `OP_REGISTER` gate; the soak mixes drivers and impostors.

use super::*;
use crate::ipc::acl::reason;
use crate::ipc::credentials::{CAP_ALL, CAP_DEV_CLAIM, CAP_IPC_CONTROL, CAP_SETUID};

const EACCES: i64 = 13;
const NET_UID: u32 = netpolicy::NET_UID;
const CARD: &str = "os.lazy.net.nic/eth0";

/// Register `name` as `slot` and return the syscall code.
fn register_as(slot: usize, name: &str) -> Result<u64, String> {
    task::harness::switch_current(slot);
    let (code, pair) = register_current(name)?;
    close_pair(pair);
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(code)
}

/// A task with exactly these credentials.
fn task_with(uid: u32, caps: u32, label: &str, session: u64) -> Result<usize, String> {
    let slot = labelled_task(label, uid, caps)?;
    let mut cred = credentials::of(slot);
    cred.session = session;
    credentials::set(slot, cred);
    Ok(slot)
}

/// Only a driver identity (no label, no session) gets a name in the
/// namespace; every other caller is refused with the namespace reason, and
/// the refusal leaves the table alone.
pub fn only_drivers_register() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let driver = task_with(NET_UID, CAP_DEV_CLAIM, "", 0)?;
        check!(
            register_as(driver, CARD)? == 0,
            "the _net driver was refused its card"
        );
        // A restarting driver (the same identity, a new task) is not the same
        // owner while the old one lives; the same task replaces its name.
        check!(
            register_as(driver, CARD)? == 0,
            "the driver could not re-register its own name"
        );
        let owner = registry::list()
            .into_iter()
            .find(|entry| entry.name == CARD)
            .ok_or("the card is not listed")?;
        check!(
            owner.owner.uid == NET_UID && owner.owner.label_id == 0 && owner.owner.session == 0,
            "List reports the owner as {:?}",
            owner.owner
        );

        // The session user, with and without every capability.
        let user = task_with(1000, 0, "", 7)?;
        let user_id = credentials::of(user).label_id;
        for name in [
            CARD,
            "os.lazy.net.nic/eth9",
            "os.lazy.net.nic",
            "os.lazy.net.nic/",
        ] {
            let code = register_as(user, name)?;
            check!(
                code == failed(EACCES),
                "a session user took {name:?} -> {code:#x}"
            );
            last_denial(user_id, reason::RESERVED_NAMESPACE)?;
        }
        for caps in [CAP_ALL, CAP_IPC_CONTROL, CAP_DEV_CLAIM, CAP_SETUID] {
            let strong = task_with(1000, caps, "", 0)?;
            let code = register_as(strong, "os.lazy.net.nic/eth9")?;
            check!(
                code == failed(EACCES),
                "uid 1000 with caps {caps:#x} took a card name -> {code:#x}"
            );
        }
        // A session-less uid that is not a driver: the stack and a service.
        for uid in [903, 901, 906, 912] {
            let other = task_with(uid, CAP_DEV_CLAIM, "", 0)?;
            check!(
                register_as(other, "os.lazy.net.nic/eth9")? == failed(EACCES),
                "uid {uid} took a card name"
            );
        }
        // A driver uid with a label or a session is not the driver.
        for (label, session) in [("app:com.evil.nic", 0), ("system:evil", 0), ("", 3)] {
            let fake = task_with(NET_UID, CAP_DEV_CLAIM, label, session)?;
            check!(
                register_as(fake, "os.lazy.net.nic/eth9")? == failed(EACCES),
                "_net with label {label:?} session {session} took a card name"
            );
        }
        // Racing the driver for the name it holds: refused by policy, and
        // the driver still owns it.
        let racer = task_with(1000, 0, "", 0)?;
        check!(
            register_as(racer, CARD)? == failed(EACCES),
            "an impostor raced the driver for eth0"
        );
        let still = registry::list()
            .into_iter()
            .find(|entry| entry.name == CARD)
            .ok_or("the card vanished")?;
        check!(
            still.owner_slot == driver,
            "the owner of eth0 is now slot {}",
            still.owner_slot
        );
        check!(
            registry::list().len() == 1,
            "a refused register touched the table ({} names)",
            registry::list().len()
        );
        Ok(())
    })
}

/// The reservation is exactly the namespace: neighbours with the same
/// spelling keep the rules they always had, and the other driver uids hold
/// names of their own.
pub fn namespace_edges() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        for uid in [netpolicy::WIFI_UID, netpolicy::WIFISIM_UID, 0] {
            let driver = task_with(uid, CAP_DEV_CLAIM, "", 0)?;
            let name = format!("os.lazy.net.nic/wlan{uid}");
            check!(register_as(driver, &name)? == 0, "driver {uid} refused");
        }
        // Not in the namespace: the ordinary `os.lazy.*` rules apply (a
        // capability-less unlabelled service is allowed, an app is not).
        let plain = task_with(1000, 0, "", 0)?;
        check!(
            register_as(plain, "os.lazy.net.nicX")? == 0,
            "os.lazy.net.nicX should follow the generic rules"
        );
        let app = task_with(1000, 0, "app:com.x.app", 0)?;
        check!(
            register_as(app, "os.lazy.net.nicX")? == failed(EACCES),
            "an app took os.lazy.net.nicX"
        );
        // A name far past the length limit is still refused for the right
        // reason by an impostor (policy runs first).
        let long = format!("os.lazy.net.nic/{}", "a".repeat(4096));
        check!(
            register_as(plain, &long)? == failed(EACCES),
            "a very long NIC name was not refused by policy"
        );
        Ok(())
    })
}

const SOAK_CYCLES: usize = 4000;

/// Thousands of attempts by drivers and impostors: exactly the drivers'
/// registrations land, the table ends where it began, no handle leaks.
pub fn soak_drivers_and_impostors() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let driver = task_with(NET_UID, CAP_DEV_CLAIM, "", 0)?;
        let wifi = task_with(netpolicy::WIFISIM_UID, CAP_DEV_CLAIM, "", 0)?;
        let impostors = [
            task_with(1000, 0, "", 5)?,
            task_with(1001, CAP_ALL, "", 0)?,
            task_with(NET_UID, CAP_DEV_CLAIM, "app:com.evil.nic", 0)?,
            task_with(NET_UID, CAP_DEV_CLAIM, "", 9)?,
        ];
        let base = handles::count_for_task(driver);
        for index in 0..SOAK_CYCLES {
            let name = format!("os.lazy.net.nic/eth{}", index % 4);
            let wlan = format!("os.lazy.net.nic/wlan{}", index % 3);
            let impostor = impostors[index % impostors.len()];
            check!(
                register_as(impostor, &name)? == failed(EACCES),
                "cycle {index}: an impostor got {name}"
            );
            check!(
                register_as(impostor, &wlan)? == failed(EACCES),
                "cycle {index}: an impostor got {wlan}"
            );
            // Each cycle the drivers replace their names (a restart).
            check!(
                register_as(driver, &name)? == 0,
                "cycle {index}: the driver was refused {name}"
            );
            check!(
                register_as(wifi, &wlan)? == 0,
                "cycle {index}: the wifi driver was refused {wlan}"
            );
            if index % 500 == 499 {
                check!(
                    registry::list().len() <= 7,
                    "cycle {index}: {} names",
                    registry::list().len()
                );
            }
        }
        for entry in registry::list() {
            check!(
                entry.owner_slot == driver || entry.owner_slot == wifi,
                "{} is owned by slot {}",
                entry.name,
                entry.owner_slot
            );
        }
        check!(
            handles::count_for_task(driver) == base,
            "the driver's handle table grew"
        );
        for slot in impostors {
            check!(
                handles::count_for_task(slot) == 0,
                "slot {slot} holds {} handles",
                handles::count_for_task(slot)
            );
        }
        Ok(())
    })
}

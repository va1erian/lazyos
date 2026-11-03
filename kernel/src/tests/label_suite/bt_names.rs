//! The Bluetooth names belong to their services (`btpolicy::names`).
//!
//! `os.lazy.usb.bt` is `usbd`'s (`_usb`): `btd` turns the packets of its
//! holder into keystrokes. `os.lazy.bt` is `btd`'s (`_bt`): Settings shows the
//! passkeys its holder sends. Both are free while their owner is away (no
//! radio at boot, a restart), so the tests drive the real `OP_REGISTER` gate
//! with every identity that might squat them; the soak mixes the owners'
//! restarts with impostors.

use super::*;
use crate::ipc::acl::reason;
use crate::ipc::credentials::{CAP_ALL, CAP_DEV_CLAIM, CAP_INPUT_SOURCE, CAP_IPC_CONTROL};
use btpolicy::names::{API_NAME, TRANSPORT_NAME};

const EACCES: i64 = 13;

fn register_as(slot: usize, name: &str) -> Result<u64, String> {
    task::harness::switch_current(slot);
    let (code, pair) = register_current(name)?;
    close_pair(pair);
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(code)
}

fn task_with(uid: u32, caps: u32, label: &str, session: u64) -> Result<usize, String> {
    let slot = labelled_task(label, uid, caps)?;
    let mut cred = credentials::of(slot);
    cred.session = session;
    credentials::set(slot, cred);
    Ok(slot)
}

/// The owners get their names and only those; every other identity (a
/// session user, root, any capability, another service, an owner uid with a
/// label or a session) is refused with the namespace reason, and the table is
/// left alone.
pub fn only_owners_register() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let usb = task_with(usbpolicy::USB_UID, CAP_DEV_CLAIM | CAP_INPUT_SOURCE, "", 0)?;
        let bt = task_with(btpolicy::BT_UID, CAP_INPUT_SOURCE, "", 0)?;
        check!(
            register_as(usb, TRANSPORT_NAME)? == 0,
            "_usb was refused its name"
        );
        check!(register_as(bt, API_NAME)? == 0, "_bt was refused its name");
        check!(
            register_as(bt, "os.lazy.usb.bt/1")? == failed(EACCES),
            "_bt took a transport name"
        );
        check!(
            register_as(usb, "os.lazy.bt/x")? == failed(EACCES),
            "_usb took btd's name"
        );
        let user = task_with(1000, 0, "", 7)?;
        let user_id = credentials::of(user).label_id;
        for name in [TRANSPORT_NAME, API_NAME, "os.lazy.usb.bt/2", "os.lazy.bt/y"] {
            check!(
                register_as(user, name)? == failed(EACCES),
                "a session user took {name:?}"
            );
            last_denial(user_id, reason::RESERVED_NAMESPACE)?;
        }
        for caps in [CAP_ALL, CAP_IPC_CONTROL, CAP_DEV_CLAIM, CAP_INPUT_SOURCE] {
            for uid in [0, 1000, netpolicy::NET_UID] {
                let strong = task_with(uid, caps, "", 0)?;
                for name in ["os.lazy.usb.bt/3", "os.lazy.bt/z"] {
                    check!(
                        register_as(strong, name)? == failed(EACCES),
                        "uid {uid} with caps {caps:#x} took {name}"
                    );
                }
            }
        }
        for (label, session) in [("app:com.evil.bt", 0), ("system:evil", 0), ("", 3)] {
            let fake = task_with(btpolicy::BT_UID, CAP_INPUT_SOURCE, label, session)?;
            check!(
                register_as(fake, "os.lazy.bt/w")? == failed(EACCES),
                "_bt with label {label:?} session {session} took a name"
            );
            let fake = task_with(usbpolicy::USB_UID, CAP_DEV_CLAIM, label, session)?;
            check!(
                register_as(fake, "os.lazy.usb.bt/4")? == failed(EACCES),
                "_usb with label {label:?} session {session} took a name"
            );
        }
        // Neighbours keep the ordinary rules.
        let plain = task_with(1000, 0, "", 0)?;
        check!(
            register_as(plain, "os.lazy.btX")? == 0,
            "os.lazy.btX should follow the generic rules"
        );
        check!(
            registry::list().len() == 3,
            "a refused register touched the table ({} names)",
            registry::list().len()
        );
        Ok(())
    })
}

const SOAK_CYCLES: usize = 3000;

/// The owners re-register (restarts, a radio that re-enumerates) while
/// impostors keep trying: only the owners' names land, nothing leaks.
pub fn soak_owners_and_impostors() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let usb = task_with(usbpolicy::USB_UID, CAP_DEV_CLAIM, "", 0)?;
        let bt = task_with(btpolicy::BT_UID, CAP_INPUT_SOURCE, "", 0)?;
        let impostors = [
            task_with(1000, 0, "", 5)?,
            task_with(0, CAP_ALL, "", 0)?,
            task_with(btpolicy::BT_UID, 0, "app:com.evil.bt", 0)?,
            task_with(usbpolicy::USB_UID, 0, "", 9)?,
        ];
        let base = handles::count_for_task(bt);
        for index in 0..SOAK_CYCLES {
            let transport = format!("{TRANSPORT_NAME}/{}", index % 3);
            let impostor = impostors[index % impostors.len()];
            for name in [transport.as_str(), API_NAME] {
                check!(
                    register_as(impostor, name)? == failed(EACCES),
                    "cycle {index}: an impostor got {name}"
                );
            }
            check!(
                register_as(usb, &transport)? == 0,
                "cycle {index}: usbd refused"
            );
            check!(
                register_as(bt, API_NAME)? == 0,
                "cycle {index}: btd refused"
            );
        }
        for entry in registry::list() {
            check!(
                entry.owner_slot == usb || entry.owner_slot == bt,
                "{} is owned by slot {}",
                entry.name,
                entry.owner_slot
            );
        }
        check!(
            handles::count_for_task(bt) == base,
            "btd's handle table grew"
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

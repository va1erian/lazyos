//! The Wi-Fi names belong to their services (`netpolicy::wifi`).
//!
//! `os.lazy.net.wifi.hw/<ifname>` is the radios' (`_wifi`, `_wifisim`):
//! `wlanmd` gives whoever holds one its frames and temporal keys.
//! `os.lazy.net.wifi` is the station manager's (`_wlan`): `AddNetwork`
//! passphrases are sent there. The tests drive the real `OP_REGISTER` gate;
//! the soak mixes the owners and impostors.

use super::*;
use crate::ipc::acl::reason;
use crate::ipc::credentials::{CAP_ALL, CAP_DEV_CLAIM, CAP_IPC_CONTROL, CAP_SETUID};

const EACCES: i64 = 13;
const RADIO: &str = "os.lazy.net.wifi.hw/wlan0";
const STATION: &str = "os.lazy.net.wifi";

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

/// The owners get their names; every other identity (a session user, root,
/// any capability, another service, an owner uid with a label or a session)
/// is refused with the namespace reason, and the table is left alone.
pub fn only_owners_register() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let chip = task_with(netpolicy::WIFI_UID, CAP_DEV_CLAIM, "", 0)?;
        let sim = task_with(netpolicy::WIFISIM_UID, 0, "", 0)?;
        let wlan = task_with(netpolicy::WLAN_UID, 0, "", 0)?;
        check!(register_as(chip, RADIO)? == 0, "_wifi was refused its radio");
        check!(
            register_as(sim, "os.lazy.net.wifi.hw/wlan1")? == 0,
            "_wifisim was refused its radio"
        );
        check!(register_as(wlan, STATION)? == 0, "_wlan was refused its name");
        // Each owner is confined to its own namespace.
        check!(
            register_as(wlan, "os.lazy.net.wifi.hw/wlan9")? == failed(EACCES),
            "_wlan took a radio name"
        );
        check!(
            register_as(chip, STATION)? == failed(EACCES),
            "_wifi took the station manager's name"
        );
        let user = task_with(1000, 0, "", 7)?;
        let user_id = credentials::of(user).label_id;
        for name in [RADIO, STATION, "os.lazy.net.wifi.hw", "os.lazy.net.wifi/x"] {
            check!(
                register_as(user, name)? == failed(EACCES),
                "a session user took {name:?}"
            );
            last_denial(user_id, reason::RESERVED_NAMESPACE)?;
        }
        for caps in [CAP_ALL, CAP_IPC_CONTROL, CAP_DEV_CLAIM, CAP_SETUID] {
            for uid in [0, 1000, netpolicy::NET_UID] {
                let strong = task_with(uid, caps, "", 0)?;
                for name in ["os.lazy.net.wifi.hw/wlan7", STATION] {
                    check!(
                        register_as(strong, name)? == failed(EACCES),
                        "uid {uid} with caps {caps:#x} took {name}"
                    );
                }
            }
        }
        for (label, session) in [("app:com.evil.wifi", 0), ("system:evil", 0), ("", 3)] {
            let fake = task_with(netpolicy::WLAN_UID, 0, label, session)?;
            check!(
                register_as(fake, "os.lazy.net.wifi/y")? == failed(EACCES),
                "_wlan with label {label:?} session {session} took a name"
            );
            let fake = task_with(netpolicy::WIFI_UID, CAP_DEV_CLAIM, label, session)?;
            check!(
                register_as(fake, "os.lazy.net.wifi.hw/wlan8")? == failed(EACCES),
                "_wifi with label {label:?} session {session} took a radio"
            );
        }
        // Neighbours keep the ordinary rules: an unlabelled service may
        // register `os.lazy.net.wifiX`, an app may not.
        let plain = task_with(1000, 0, "", 0)?;
        check!(
            register_as(plain, "os.lazy.net.wifiX")? == 0,
            "os.lazy.net.wifiX should follow the generic rules"
        );
        check!(
            registry::list().len() == 4,
            "a refused register touched the table ({} names)",
            registry::list().len()
        );
        Ok(())
    })
}

const SOAK_CYCLES: usize = 3000;

/// The owners re-register (restarts) while impostors keep trying: only the
/// owners' names land, nothing leaks.
pub fn soak_owners_and_impostors() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let sim = task_with(netpolicy::WIFISIM_UID, 0, "", 0)?;
        let wlan = task_with(netpolicy::WLAN_UID, 0, "", 0)?;
        let impostors = [
            task_with(1000, 0, "", 5)?,
            task_with(0, CAP_ALL, "", 0)?,
            task_with(netpolicy::WLAN_UID, 0, "app:com.evil.wlan", 0)?,
            task_with(netpolicy::WIFISIM_UID, 0, "", 9)?,
        ];
        let base = handles::count_for_task(wlan);
        for index in 0..SOAK_CYCLES {
            let radio = format!("os.lazy.net.wifi.hw/wlan{}", index % 3);
            let impostor = impostors[index % impostors.len()];
            for name in [radio.as_str(), STATION] {
                check!(
                    register_as(impostor, name)? == failed(EACCES),
                    "cycle {index}: an impostor got {name}"
                );
            }
            check!(register_as(sim, &radio)? == 0, "cycle {index}: sim refused");
            check!(register_as(wlan, STATION)? == 0, "cycle {index}: wlan refused");
        }
        for entry in registry::list() {
            check!(
                entry.owner_slot == sim || entry.owner_slot == wlan,
                "{} is owned by slot {}",
                entry.name,
                entry.owner_slot
            );
        }
        check!(
            handles::count_for_task(wlan) == base,
            "wlanmd's handle table grew"
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

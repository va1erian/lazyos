//! `os.lazy.net.wifi.v1`: decode, authorize (`wlansta::policy`, from the
//! kernel-stamped sender), act, encode.
//!
//! A profile is named by its owner and its id: a request's id is looked up
//! only among the profiles its caller may use (`policy::resolve`), and the
//! station is told the owner too, so a user can neither join nor block
//! another user's network by reusing its id.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::server::{Caller as Stamped, Fault};
use user::messenger::wifi::{self, api};
use user::messenger::{errno, Error, Result as MResult};
use user::sys;
use wlansta::policy::{self, Caller, Denied};
use wlansta::{profile, Owner, Profile, Refused, Security};

use crate::radio::{wire_state, Slot};
use crate::{now_ms, Daemon};

/// Ticks one caller waits between two `Scan`s (a scan costs every channel's
/// dwell, and it pauses traffic while joined).
const SCAN_INTERVAL_TICKS: u64 = 100;
/// Callers remembered for the scan rate limit; the oldest goes first.
const SCAN_CALLERS: usize = 32;

/// A refusal with the service's reason, so a caller can tell a policy
/// `EPERM` from any other.
fn refuse(code: i64) -> Error {
    Error::Errno(-code)
}

fn denied(denial: Denied) -> Error {
    refuse(match denial {
        Denied::Perm => errno::EPERM,
        Denied::Invalid => errno::EINVAL,
        Denied::Missing => errno::ENOENT,
    })
}

fn refused(refusal: Refused) -> Error {
    refuse(match refusal {
        Refused::Busy => errno::EBUSY,
        Refused::NoProfile => errno::ENOENT,
        Refused::Invalid => errno::EINVAL,
    })
}

/// The policy's view of the kernel-stamped sender.
fn caller_of(cx: &Stamped) -> Caller {
    Caller {
        uid: cx.uid,
        elevd: elevpolicy::is_elevd(cx.uid, cx.label_id, cx.session),
    }
}

fn slot<'a>(radios: &'a mut [Slot], ifname: &str) -> MResult<&'a mut Slot> {
    radios
        .iter_mut()
        .find(|slot| slot.ifname == ifname)
        .ok_or(refuse(errno::ENODEV))
}

/// The generated server of `os.lazy.net.wifi.v1`: each method is decode-free;
/// policy decides from the kernel-stamped sender.
impl api::Server for Daemon {
    fn interfaces(&mut self, _: &Stamped) -> Result<Vec<String>, Fault> {
        Ok(self.radios.iter().map(|s| s.ifname.clone()).collect())
    }

    fn scan(&mut self, cx: &Stamped, ifname: String, ssids: Vec<String>) -> Result<(), Fault> {
        Ok(scan(self, caller_of(cx), &ifname, ssids)?)
    }

    fn networks(&mut self, cx: &Stamped, ifname: String) -> Result<Vec<api::Network>, Fault> {
        let caller = caller_of(cx);
        let list = slot(&mut self.radios, &ifname)?
            .station
            .networks(policy::viewer(caller))
            .into_iter()
            .map(|n| api::Network {
                ssid: n.ssid,
                security: n.security as u32,
                rssi_dbm: n.rssi_dbm,
                freq_mhz: n.freq_mhz,
                bss_count: n.bss_count,
                id: n.id,
                connected: n.connected,
            })
            .collect();
        Ok(list)
    }

    fn add_network(
        &mut self,
        cx: &Stamped,
        profile: api::Profile,
        passphrase: Option<String>,
    ) -> Result<String, Fault> {
        Ok(add_network(self, caller_of(cx), profile, passphrase)?)
    }

    fn connect(&mut self, cx: &Stamped, ifname: String, id: String) -> Result<(), Fault> {
        Ok(connect(self, caller_of(cx), &ifname, &id)?)
    }

    fn disconnect(&mut self, cx: &Stamped, ifname: String) -> Result<(), Fault> {
        let caller = caller_of(cx);
        let slot = slot(&mut self.radios, &ifname)?;
        if !policy::may_disconnect(caller, slot.station.current_owner()) {
            return Err(refuse(errno::EPERM).into());
        }
        let actions = slot.station.disconnect(now_ms());
        slot.execute(&mut self.store, actions);
        Ok(())
    }

    fn forget(&mut self, cx: &Stamped, id: String) -> Result<(), Fault> {
        Ok(forget(self, caller_of(cx), &id)?)
    }

    fn status(&mut self, cx: &Stamped, ifname: String) -> Result<api::WifiState, Fault> {
        let status = slot(&mut self.radios, &ifname)?.station.status();
        Ok(wire_state(status, policy::viewer(caller_of(cx))))
    }
}

/// `Scan`, at most once per [`SCAN_INTERVAL_TICKS`] per caller (`EAGAIN`).
fn scan(daemon: &mut Daemon, caller: Caller, ifname: &str, ssids: Vec<String>) -> MResult<()> {
    let now = sys::clock();
    if let Some((_, at)) = daemon.scans.iter().find(|(uid, _)| *uid == caller.uid) {
        if now.saturating_sub(*at) < SCAN_INTERVAL_TICKS {
            return Err(refuse(errno::EAGAIN));
        }
    }
    let slot = slot(&mut daemon.radios, ifname)?;
    let ssids = ssids.into_iter().map(String::into_bytes).collect();
    let actions = slot.station.scan(now_ms(), ssids).map_err(refused)?;
    slot.execute(&mut daemon.store, actions);
    daemon.scans.retain(|(uid, _)| *uid != caller.uid);
    if daemon.scans.len() >= SCAN_CALLERS {
        daemon.scans.remove(0);
    }
    daemon.scans.push((caller.uid, now));
    Ok(())
}

/// The SSID a new profile names: as typed for a hidden network, else the
/// raw octets of the heard network the typed text names (`Station::
/// heard_ssid`: the octets themselves first, then the one SSID that displays
/// so), so a name shown with invisible characters stripped is still joined.
fn raw_ssid(daemon: &Daemon, typed: String, hidden: bool) -> MResult<Vec<u8>> {
    let typed = typed.into_bytes();
    if hidden {
        return Ok(typed);
    }
    let mut found: Option<Vec<u8>> = None;
    for slot in &daemon.radios {
        match slot.station.heard_ssid(&typed) {
            Ok(Some(raw)) if raw == typed => return Ok(raw),
            Ok(Some(raw)) => match &found {
                Some(other) if *other != raw => return Err(refuse(errno::EINVAL)),
                _ => found = Some(raw),
            },
            Ok(None) => {}
            Err(_) => return Err(refuse(errno::EINVAL)),
        }
    }
    match found {
        // A profile's SSID is stored as text: one that is not UTF-8 cannot
        // be remembered faithfully.
        Some(raw) if core::str::from_utf8(&raw).is_err() => Err(refuse(errno::EINVAL)),
        Some(raw) => Ok(raw),
        None => Ok(typed),
    }
}

/// `AddNetwork`: check, pick an id, keep the secret and the profile. The
/// passphrase is wiped here whatever the outcome.
fn add_network(
    daemon: &mut Daemon,
    caller: Caller,
    wire: api::Profile,
    mut passphrase: Option<String>,
) -> MResult<String> {
    let result = add_checked(daemon, caller, wire, passphrase.as_deref());
    if let Some(text) = passphrase.take() {
        wifi::wipe(text);
    }
    let profile = result?;
    sys::write_str(&alloc::format!(
        "WLANMD:ADD {} owner={}\n",
        profile.id,
        owner_text(profile.owner)
    ));
    daemon.reload_profiles();
    Ok(profile.id)
}

fn add_checked(
    daemon: &mut Daemon,
    caller: Caller,
    wire: api::Profile,
    passphrase: Option<&str>,
) -> MResult<Profile> {
    let owner = policy::add_owner(caller, wire.scope).map_err(denied)?;
    let security = Security::from_wire(wire.security).ok_or(refuse(errno::EINVAL))?;
    policy::check_new(owner, &wire.id, &wire.ssid, security, passphrase).map_err(denied)?;
    let ssid = raw_ssid(daemon, wire.ssid, wire.hidden)?;
    let exists = daemon
        .store
        .profiles()
        .iter()
        .any(|p| p.owner == owner && (p.ssid == ssid || p.id == wire.id));
    if exists {
        return Err(refuse(errno::EEXIST));
    }
    // A system network's secret is already in `keyd` under the id `elevd`
    // chose; a user's network gets a fresh id.
    let id = match owner {
        Owner::System => wire.id,
        Owner::User(_) => {
            let mut random = [0u8; 6];
            sys::random(&mut random).map_err(Error::Errno)?;
            profile::new_id(random)
        }
    };
    let profile = Profile {
        id,
        ssid,
        security,
        hidden: wire.hidden,
        autoconnect: wire.autoconnect,
        priority: wire.priority,
        owner,
    };
    daemon.store.add(&profile, passphrase)?;
    Ok(profile)
}

fn owner_text(owner: Owner) -> String {
    match owner {
        Owner::System => String::from("system"),
        Owner::User(uid) => alloc::format!("{uid}"),
    }
}

/// `Connect`: the caller's profile of that id (a system network is
/// anyone's, a user network its owner's).
fn connect(daemon: &mut Daemon, caller: Caller, ifname: &str, id: &str) -> MResult<()> {
    let owner = policy::resolve(caller, daemon.store.profiles(), id)
        .map_err(denied)?
        .owner;
    let slot = slot(&mut daemon.radios, ifname)?;
    let actions = slot.station.connect(now_ms(), owner, id).map_err(refused)?;
    slot.execute(&mut daemon.store, actions);
    Ok(())
}

/// `Forget`: the owner (or `elevd` for a system network) deletes it.
fn forget(daemon: &mut Daemon, caller: Caller, id: &str) -> MResult<()> {
    let found = policy::resolve_forget(caller, daemon.store.profiles(), id).map_err(denied)?;
    let Some(profile) = found.cloned() else {
        return Ok(());
    };
    daemon.store.erase(&profile);
    sys::write_str(&alloc::format!(
        "WLANMD:FORGET {id} owner={}\n",
        owner_text(profile.owner)
    ));
    daemon.reload_profiles();
    Ok(())
}

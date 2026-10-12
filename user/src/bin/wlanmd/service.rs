//! `os.lazy.net.wifi.v1`: decode, authorize (`wlansta::policy`, from the
//! kernel-stamped sender), act, encode.
//!
//! A profile is named by its owner and its id: a request's id is looked up
//! only among the profiles its caller may use (`policy::resolve`), and the
//! station is told the owner too, so a user can neither join nor block
//! another user's network by reusing its id.

use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::wifi::{self, api};
use user::messenger::{errno, Error, Message, Parcel, Result};
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

fn parse(error: libmessenger::Error) -> Error {
    Error::Parcel(error)
}

fn caller_of(message: &Message) -> Caller {
    let cred = message.caller();
    Caller {
        uid: cred.uid,
        elevd: elevpolicy::is_elevd(cred.uid, cred.label_id, cred.session),
    }
}

fn slot<'a>(radios: &'a mut [Slot], ifname: &str) -> Result<&'a mut Slot> {
    radios
        .iter_mut()
        .find(|slot| slot.ifname == ifname)
        .ok_or(refuse(errno::ENODEV))
}

/// Route one request; `Err` becomes the structured error reply.
pub(crate) fn dispatch(daemon: &mut Daemon, message: &Message) -> Result<Parcel> {
    if message.interface_id() != api::INTERFACE_ID {
        return Err(refuse(errno::EINVAL));
    }
    let caller = caller_of(message);
    let body = &message.parcel.body;
    let method = message.method();
    let reply = match method {
        api::METHOD_INTERFACES => {
            let names = daemon.radios.iter().map(|s| s.ifname.clone()).collect();
            api::encode_interfaces_reply(&api::InterfacesReply { names })
        }
        api::METHOD_SCAN => {
            let args = api::decode_scan_args(body).map_err(parse)?;
            scan(daemon, caller, &args.ifname, args.ssids)?;
            Ok(Vec::new())
        }
        api::METHOD_NETWORKS => {
            let args = api::decode_networks_args(body).map_err(parse)?;
            let list = slot(&mut daemon.radios, &args.ifname)?
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
            api::encode_networks_reply(&api::NetworksReply { list })
        }
        api::METHOD_ADDNETWORK => {
            let args = api::decode_add_network_args(body).map_err(parse)?;
            let id = add_network(daemon, caller, args.profile, args.passphrase)?;
            api::encode_add_network_reply(&api::AddNetworkReply { id })
        }
        api::METHOD_CONNECT => {
            let args = api::decode_connect_args(body).map_err(parse)?;
            connect(daemon, caller, &args.ifname, &args.id)?;
            Ok(Vec::new())
        }
        api::METHOD_DISCONNECT => {
            let args = api::decode_disconnect_args(body).map_err(parse)?;
            let slot = slot(&mut daemon.radios, &args.ifname)?;
            if !policy::may_disconnect(caller, slot.station.current_owner()) {
                return Err(refuse(errno::EPERM));
            }
            let actions = slot.station.disconnect(now_ms());
            slot.execute(&mut daemon.store, actions);
            Ok(Vec::new())
        }
        api::METHOD_FORGET => {
            let args = api::decode_forget_args(body).map_err(parse)?;
            forget(daemon, caller, &args.id)?;
            Ok(Vec::new())
        }
        api::METHOD_STATUS => {
            let args = api::decode_status_args(body).map_err(parse)?;
            let status = slot(&mut daemon.radios, &args.ifname)?.station.status();
            let status = wire_state(status, policy::viewer(caller));
            api::encode_status_reply(&api::StatusReply { status })
        }
        _ => return Err(refuse(errno::EINVAL)),
    };
    Ok(wifi::reply(method, reply.map_err(parse)?))
}

/// `Scan`, at most once per [`SCAN_INTERVAL_TICKS`] per caller (`EAGAIN`).
fn scan(daemon: &mut Daemon, caller: Caller, ifname: &str, ssids: Vec<String>) -> Result<()> {
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
fn raw_ssid(daemon: &Daemon, typed: String, hidden: bool) -> Result<Vec<u8>> {
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
) -> Result<String> {
    let result = add_checked(daemon, caller, wire, passphrase.as_deref());
    if let Some(text) = passphrase.take() {
        let mut bytes = text.into_bytes();
        bytes.fill(0);
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
) -> Result<Profile> {
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
fn connect(daemon: &mut Daemon, caller: Caller, ifname: &str, id: &str) -> Result<()> {
    let owner = policy::resolve(caller, daemon.store.profiles(), id)
        .map_err(denied)?
        .owner;
    let slot = slot(&mut daemon.radios, ifname)?;
    let actions = slot.station.connect(now_ms(), owner, id).map_err(refused)?;
    slot.execute(&mut daemon.store, actions);
    Ok(())
}

/// `Forget`: the owner (or `elevd` for a system network) deletes it.
fn forget(daemon: &mut Daemon, caller: Caller, id: &str) -> Result<()> {
    let found = policy::resolve_forget(caller, daemon.store.profiles(), id).map_err(denied)?;
    let Some(profile) = found.cloned() else {
        return Ok(());
    };
    daemon.store.erase(&profile);
    sys::write_str(&alloc::format!("WLANMD:FORGET {id} owner={}\n", owner_text(profile.owner)));
    daemon.reload_profiles();
    Ok(())
}

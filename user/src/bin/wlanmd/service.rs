//! `os.lazy.net.wifi.v1`: decode, authorize (`wlansta::policy`, from the
//! kernel-stamped sender), act, encode.

use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::wifi::{self, api};
use user::messenger::{errno, Error, Message, Parcel, Result};
use user::sys;
use wlansta::policy::{self, Caller, Denied};
use wlansta::{profile, Owner, Profile, Refused, Security};

use crate::radio::{wire_state, Slot};
use crate::{now_ms, Daemon};

/// A refusal with the service's reason, so a caller can tell a policy
/// `EPERM` from any other.
fn refuse(code: i64) -> Error {
    Error::Errno(-code)
}

fn denied(denial: Denied) -> Error {
    refuse(match denial {
        Denied::Perm => errno::EPERM,
        Denied::Invalid => errno::EINVAL,
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

fn slot<'a>(daemon: &'a mut Daemon, ifname: &str) -> Result<&'a mut Slot> {
    daemon
        .radios
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
            let ssids = args.ssids.into_iter().map(String::into_bytes).collect();
            let store = &mut daemon.store;
            let slot = daemon
                .radios
                .iter_mut()
                .find(|slot| slot.ifname == args.ifname)
                .ok_or(refuse(errno::ENODEV))?;
            let actions = slot.station.scan(now_ms(), ssids).map_err(refused)?;
            slot.execute(store, actions);
            Ok(Vec::new())
        }
        api::METHOD_NETWORKS => {
            let args = api::decode_networks_args(body).map_err(parse)?;
            let list = slot(daemon, &args.ifname)?
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
            let store = &mut daemon.store;
            let slot = daemon
                .radios
                .iter_mut()
                .find(|slot| slot.ifname == args.ifname)
                .ok_or(refuse(errno::ENODEV))?;
            let actions = slot.station.disconnect(now_ms());
            slot.execute(store, actions);
            Ok(Vec::new())
        }
        api::METHOD_FORGET => {
            let args = api::decode_forget_args(body).map_err(parse)?;
            forget(daemon, caller, &args.id)?;
            Ok(Vec::new())
        }
        api::METHOD_STATUS => {
            let args = api::decode_status_args(body).map_err(parse)?;
            let status = wire_state(slot(daemon, &args.ifname)?.station.status());
            api::encode_status_reply(&api::StatusReply { status })
        }
        _ => return Err(refuse(errno::EINVAL)),
    };
    Ok(wifi::reply(method, reply.map_err(parse)?))
}

/// `AddNetwork`: check, pick an id, keep the secret and the profile.
fn add_network(
    daemon: &mut Daemon,
    caller: Caller,
    wire: api::Profile,
    passphrase: Option<String>,
) -> Result<String> {
    let owner = policy::add_owner(caller, wire.scope).map_err(denied)?;
    let security = Security::from_wire(wire.security).ok_or(refuse(errno::EINVAL))?;
    if !wire.id.is_empty() {
        return Err(refuse(errno::EINVAL));
    }
    policy::check_new(&wire.ssid, security, passphrase.as_deref()).map_err(denied)?;
    let ssid = wire.ssid.into_bytes();
    let exists = daemon
        .store
        .profiles()
        .iter()
        .any(|p| p.owner == owner && p.ssid == ssid);
    if exists {
        return Err(refuse(errno::EEXIST));
    }
    let mut random = [0u8; 6];
    sys::random(&mut random).map_err(Error::Errno)?;
    let profile = Profile {
        id: profile::new_id(random),
        ssid,
        security,
        hidden: wire.hidden,
        autoconnect: wire.autoconnect,
        priority: wire.priority,
        owner,
    };
    let mut passphrase = passphrase;
    let result = daemon.store.add(&profile, passphrase.as_deref());
    // The passphrase is not kept here a moment longer than the call.
    if let Some(text) = passphrase.take() {
        let mut bytes = text.into_bytes();
        bytes.fill(0);
    }
    result?;
    sys::write_str(&alloc::format!(
        "WLANMD:ADD {} owner={}\n",
        profile.id,
        owner_text(owner)
    ));
    daemon.reload_profiles();
    Ok(profile.id)
}

fn owner_text(owner: Owner) -> String {
    match owner {
        Owner::System => String::from("system"),
        Owner::User(uid) => alloc::format!("{uid}"),
    }
}

/// `Connect`: a system network is anyone's, a user network its owner's.
fn connect(daemon: &mut Daemon, caller: Caller, ifname: &str, id: &str) -> Result<()> {
    let owner = daemon
        .store
        .profiles()
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.owner)
        .ok_or(refuse(errno::ENOENT))?;
    if !policy::may_use(caller, owner) {
        return Err(refuse(errno::EPERM));
    }
    let store = &mut daemon.store;
    let slot = daemon
        .radios
        .iter_mut()
        .find(|slot| slot.ifname == ifname)
        .ok_or(refuse(errno::ENODEV))?;
    let actions = slot.station.connect(now_ms(), id).map_err(refused)?;
    slot.execute(store, actions);
    Ok(())
}

/// `Forget`: the owner (or `elevd` for a system network) deletes it.
fn forget(daemon: &mut Daemon, caller: Caller, id: &str) -> Result<()> {
    let Some(profile) = daemon.store.profiles().iter().find(|p| p.id == id).cloned() else {
        return Ok(());
    };
    if !policy::may_forget(caller, profile.owner) {
        return Err(refuse(errno::EPERM));
    }
    daemon.store.erase(&profile);
    sys::write_str(&alloc::format!("WLANMD:FORGET {id}\n"));
    daemon.reload_profiles();
    Ok(())
}

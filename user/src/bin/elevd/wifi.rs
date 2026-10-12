//! `net.wifi.add` and `net.wifi.forget`, as `elevd` performs them
//! (`elevpolicy::wifi`).
//!
//! The secret and the profile are two services' business (`keyd` keeps a
//! `system` secret only from `elevd`, `wlanmd` takes a `System` profile only
//! from `elevd` and without a passphrase), so this is the one place that
//! does both, in an order that never leaves a profile without its secret:
//! the secret first, the profile second, and the secret removed again if the
//! profile is refused. The id is made here from the kernel's random source;
//! the asker never chooses it.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use user::messenger::wifi::{self, api};
use user::messenger::{errno, keyd};
use user::sys;

use super::perform::Done;
use super::Refusal;

fn keys() -> Result<keyd::Client, Refusal> {
    keyd::Client::connect()
        .map_err(|_| Refusal::new(errno::ENOENT, "the key service is not running"))
}

fn station() -> Result<wifi::Client, Refusal> {
    wifi::Client::connect()
        .map_err(|_| Refusal::new(errno::ENOENT, "the Wi-Fi service is not running"))
}

/// Keep the network for every user. Returns its id as the one value, so the
/// asker can join it.
pub(crate) fn add(ssid: &str, wpa2: bool, hidden: bool, passphrase: &str) -> Result<Done, Refusal> {
    let mut random = [0u8; 6];
    sys::random(&mut random).map_err(|_| Refusal::new(errno::EIO, "no randomness"))?;
    let id = wlansta::profile::new_id(random);
    let station = station()?;
    let keys = keys()?;
    if wpa2 {
        keys.store_secret("system", &id, passphrase.as_bytes())
            .map_err(Refusal::of)?;
    }
    let profile = api::Profile {
        id: id.clone(),
        ssid: String::from(ssid),
        security: if wpa2 {
            api::SECURITY_WPA2_PSK
        } else {
            api::SECURITY_OPEN
        },
        hidden,
        autoconnect: true,
        priority: 0,
        scope: api::SCOPE_SYSTEM,
    };
    if let Err(error) = station.add_network(profile, None) {
        if wpa2 {
            let _ = keys.delete_secret("system", &id);
        }
        return Err(Refusal::of(error));
    }
    Ok((
        format!("Saved the Wi-Fi network for all users as {id}"),
        vec![id],
    ))
}

/// Forget the network `id` of every user, then its secret. A profile that
/// is gone is not an error, and neither is a secret that is gone.
pub(crate) fn forget(id: &str) -> Result<Done, Refusal> {
    station()?.forget(id).map_err(Refusal::of)?;
    if let Err(error) = keys()?.delete_secret("system", id) {
        if error.errno() != Some(-errno::ENOENT) {
            return Err(Refusal::of(error));
        }
    }
    Ok((format!("Forgot the Wi-Fi network {id}"), Vec::new()))
}

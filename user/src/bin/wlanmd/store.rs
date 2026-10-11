//! Profiles in `confd` and passphrases in `keyd` (docs/wifi-prerequisites-plan.md
//! section 3.7). The key layout and its checks are `wlansta::profile`
//! (host-tested); this file does the calls.
//!
//! `confd` lets `_wlan` read and write the Wi-Fi profile subtrees of the
//! machine and of every user, and nothing else (`confd::Caller::wifi`).
//! `keyd` keeps the passphrase under the profile's id, in the profile's scope
//! (`StoreSecretFor`), and answers `WifiPmk` to `_wlan` alone.

use alloc::string::String;
use alloc::vec::Vec;

use confd::Value as ConfValue;
use user::messenger::{confd as conf, errno, keyd, Error};
use wlansta::profile::{self, Value};
use wlansta::{Key, Owner, PmkError, Profile};

/// Ticks a `confd` call may take.
const CONFD_TICKS: u64 = 200;
/// Profiles kept at most (system and every user together).
const MAX_PROFILES: usize = 64;

pub(crate) struct Store {
    conf: Option<conf::Client>,
    keyd: Option<keyd::Client>,
    profiles: Vec<Profile>,
    /// The profiles were read once (a `confd` that was not up yet is asked
    /// again on the next look).
    pub loaded: bool,
}

/// The `keyd` scope and owner uid of a profile's secret.
fn scope_of(owner: Owner) -> (&'static str, u32) {
    match owner {
        Owner::System => ("system", 0),
        Owner::User(uid) => ("user", uid),
    }
}

fn to_conf(value: Value) -> ConfValue {
    match value {
        Value::Str(text) => ConfValue::Str(text),
        Value::Bool(flag) => ConfValue::Bool(flag),
        Value::U64(number) => ConfValue::U64(number),
    }
}

fn from_conf(value: ConfValue) -> Option<Value> {
    match value {
        ConfValue::Str(text) => Some(Value::Str(text)),
        ConfValue::Bool(flag) => Some(Value::Bool(flag)),
        ConfValue::U64(number) => Some(Value::U64(number)),
        _ => None,
    }
}

impl Store {
    pub fn new() -> Store {
        Store {
            conf: None,
            keyd: None,
            profiles: Vec::new(),
            loaded: false,
        }
    }

    fn conf(&mut self) -> Option<&conf::Client> {
        if self.conf.is_none() {
            self.conf = conf::Client::connect()
                .ok()
                .map(|client| client.with_timeout(CONFD_TICKS));
        }
        self.conf.as_ref()
    }

    fn keyd(&mut self) -> Option<&keyd::Client> {
        if self.keyd.is_none() {
            self.keyd = keyd::Client::connect().ok();
        }
        self.keyd.as_ref()
    }

    /// The regulatory domain for the radios.
    pub fn country(&mut self) -> String {
        let configured = match self.conf().map(|c| c.get(profile::COUNTRY_KEY)) {
            Some(Ok(Some(ConfValue::Str(code)))) => Some(code),
            _ => None,
        };
        String::from(profile::country(configured.as_deref()))
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Read every profile again; on failure keep what was known.
    pub fn reload(&mut self) {
        let Some(client) = self.conf() else {
            return;
        };
        let mut keys = match client.list(profile::SYSTEM_PREFIX) {
            Ok(keys) => keys,
            Err(error) if error.errno() == Some(-errno::ENOENT) => Vec::new(),
            Err(_) => {
                self.conf = None;
                return;
            }
        };
        // Every user's profiles: `confd` shows `_wlan` the Wi-Fi subtrees.
        keys.extend(client.list("user").unwrap_or_default());
        let mut profiles = Vec::new();
        for (owner, id) in profile::ids(&keys).into_iter().take(MAX_PROFILES) {
            let get = |field: &str| {
                let value = client.get(&profile::key(owner, &id, field)).ok()??;
                from_conf(value)
            };
            if let Some(profile) = profile::parse(owner, &id, get) {
                profiles.push(profile);
            }
        }
        self.profiles = profiles;
        self.loaded = true;
    }

    /// Remember `profile`: the secret first, then the fields (the SSID last,
    /// so a reader never sees half a profile); undone if a step fails.
    pub fn add(&mut self, profile: &Profile, passphrase: Option<&str>) -> Result<(), Error> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(Error::Errno(-errno::ENOSPC));
        }
        let (scope, uid) = scope_of(profile.owner);
        if let Some(secret) = passphrase {
            let keyd = self.keyd().ok_or(Error::Errno(-errno::EAGAIN))?;
            keyd.store_secret_for(scope, &profile.id, secret.as_bytes(), uid)?;
        }
        let written = self.write_fields(profile);
        if written.is_err() {
            self.erase(profile);
        }
        written
    }

    fn write_fields(&mut self, profile: &Profile) -> Result<(), Error> {
        let client = self.conf().ok_or(Error::Errno(-errno::EAGAIN))?;
        for (field, value) in profile::fields(profile) {
            client.set(&profile::key(profile.owner, &profile.id, field), &to_conf(value))?;
        }
        Ok(())
    }

    /// Delete a profile's fields and its secret. Missing pieces are fine.
    pub fn erase(&mut self, profile: &Profile) {
        if let Some(client) = self.conf() {
            for field in profile::FIELDS.iter().rev() {
                let _ = client.delete(&profile::key(profile.owner, &profile.id, field));
            }
        }
        let (scope, uid) = scope_of(profile.owner);
        if profile.security == wlansta::Security::Wpa2Psk {
            if let Some(keyd) = self.keyd() {
                let _ = keyd.delete_secret_for(scope, &profile.id, uid);
            }
        }
    }

    /// The PMK of profile `id` for `ssid`, from `keyd`.
    pub fn pmk(&mut self, id: &str, owner: Owner, ssid: &[u8]) -> Result<Key<32>, PmkError> {
        let (scope, uid) = scope_of(owner);
        let Some(keyd) = self.keyd() else {
            return Err(PmkError::Unavailable);
        };
        match keyd.wifi_pmk(scope, id, ssid, uid) {
            Ok(mut bytes) => {
                let pmk = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| PmkError::Missing);
                bytes.fill(0);
                pmk.map(Key::new)
            }
            Err(error) => match error.errno().map(|code| -code) {
                Some(errno::ENOENT | errno::EINVAL | errno::EPERM) => Err(PmkError::Missing),
                _ => {
                    self.keyd = None;
                    Err(PmkError::Unavailable)
                }
            },
        }
    }
}

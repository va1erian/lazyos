//! Profiles in `confd` and passphrases in `keyd` (docs/wifi-prerequisites-plan.md
//! section 3.7). The key layout and its checks are `wlansta::profile`
//! (host-tested); this file does the calls.
//!
//! `confd` lets `_wlan` read and write the Wi-Fi profile subtrees of the
//! machine and of every user, and nothing else (`confd::Caller::wifi`).
//! `keyd` keeps a user network's passphrase under the profile's id in the
//! `user` scope (`StoreSecretFor`, which `keyd` refuses `wlanmd` in the
//! `system` scope: a system network's secret is `elevd`'s to store, through
//! `net.wifi.system`, under the same id), and answers `WifiPmk` to `_wlan`
//! alone, for profile ids only.

use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use confd::Value as ConfValue;
use user::messenger::{confd as conf, errno, keyd, Error};
use wlansta::profile::{self, Value};
use wlansta::{Key, Owner, PmkError, Profile};

/// Ticks a `confd` read may take.
const CONFD_TICKS: u64 = 200;
/// Ticks a `confd` write may take: the first write to a user's subtree
/// creates its store on disk.
const CONFD_WRITE_TICKS: u64 = 1_000;

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

    /// The regulatory domain for the radios: `None` when `confd` could not
    /// be asked (the radios keep theirs).
    pub fn country(&mut self) -> Option<String> {
        let configured = match self.conf()?.get(profile::COUNTRY_KEY) {
            Ok(Some(ConfValue::Str(code))) => Some(code),
            Ok(_) => None,
            // Unset, or refused: the world domain, which needs nobody's word.
            Err(error)
                if [errno::ENOENT, errno::EPERM, errno::EACCES, errno::EINVAL]
                    .iter()
                    .any(|code| error.errno() == Some(-code)) =>
            {
                None
            }
            Err(_) => {
                self.conf = None;
                return None;
            }
        };
        // The radio takes only an assigned ISO 3166-1 code (or `00`): the
        // same table decides here, so a typo means the world domain rather
        // than a radio that refuses every code.
        let code = profile::country(configured.as_deref());
        let known = wifimac::regdom::Domain::parse(code).is_some();
        Some(String::from(if known { code } else { profile::WORLD }))
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Read every profile again. Any `confd` error (a timeout, a restart)
    /// keeps the list that was known and is returned: replacing it with a
    /// partial one would make the station leave a network whose profile only
    /// failed to read.
    pub fn reload(&mut self) -> Result<(), Error> {
        let profiles = self.read_all();
        if profiles.is_err() {
            self.conf = None;
        }
        self.profiles = profiles?;
        self.loaded = true;
        Ok(())
    }

    fn read_all(&mut self) -> Result<Vec<Profile>, Error> {
        let client = self.conf().ok_or(Error::Errno(-errno::EAGAIN))?;
        let list = |prefix: &str| match client.list(prefix) {
            Err(error) if error.errno() == Some(-errno::ENOENT) => Ok(Vec::new()),
            other => other,
        };
        let mut keys = list(profile::SYSTEM_PREFIX)?;
        // Every user's profiles: `confd` shows `_wlan` the Wi-Fi subtrees.
        keys.extend(list("user")?);
        let mut profiles = Vec::new();
        for (owner, id) in profile::ids(&keys, profile::MAX_READ_PER_OWNER) {
            let mut fields: Vec<(&str, Option<Value>)> = Vec::new();
            for field in profile::FIELDS {
                let value = match client.get(&profile::key(owner, &id, field)) {
                    Ok(value) => value.and_then(from_conf),
                    // Deleted between the list and the read: half a profile.
                    Err(error) if error.errno() == Some(-errno::ENOENT) => None,
                    Err(error) => return Err(error),
                };
                fields.push((field, value));
            }
            let get = |name: &str| {
                fields
                    .iter()
                    .find(|(f, _)| *f == name)
                    .and_then(|(_, v)| v.clone())
            };
            if let Some(profile) = profile::parse(owner, &id, get) {
                profiles.push(profile);
            }
        }
        Ok(profile::normalize(profiles, profile::MAX_PER_OWNER))
    }

    /// Remember `profile`: the secret first, then the fields (the SSID last,
    /// so a reader never sees half a profile); undone if a step fails. An
    /// owner holds at most `profile::MAX_PER_OWNER` profiles (`ENOSPC`).
    pub fn add(&mut self, profile: &Profile, passphrase: Option<&str>) -> Result<(), Error> {
        let owned = self
            .profiles
            .iter()
            .filter(|p| p.owner == profile.owner)
            .count();
        if owned >= profile::MAX_PER_OWNER {
            return Err(Error::Errno(-errno::ENOSPC));
        }
        let (scope, uid) = scope_of(profile.owner);
        if let Some(secret) = passphrase {
            let keyd = self.keyd().ok_or(Error::Errno(-errno::EAGAIN))?;
            if let Err(error) = keyd.store_secret_for(scope, &profile.id, secret.as_bytes(), uid) {
                trace::line!(
                    trace::Channel::Wlanmd,
                    "ADD:FAIL {} keyd {}",
                    profile.id,
                    error.message()
                );
                return Err(error);
            }
        }
        let written = self.write_fields(profile);
        if let Err(error) = &written {
            trace::line!(
                trace::Channel::Wlanmd,
                "ADD:FAIL {} confd {}",
                profile.id,
                error.message()
            );
            self.erase(profile);
        }
        written
    }

    fn write_fields(&mut self, profile: &Profile) -> Result<(), Error> {
        self.conf().ok_or(Error::Errno(-errno::EAGAIN))?;
        self.retime(CONFD_WRITE_TICKS);
        let written = self.set_fields(profile);
        self.retime(CONFD_TICKS);
        written
    }

    fn set_fields(&mut self, profile: &Profile) -> Result<(), Error> {
        let client = self.conf().ok_or(Error::Errno(-errno::EAGAIN))?;
        for (field, value) in profile::fields(profile) {
            client.set(
                &profile::key(profile.owner, &profile.id, field),
                &to_conf(value),
            )?;
        }
        Ok(())
    }

    /// Give the `confd` client a new per-call timeout.
    fn retime(&mut self, ticks: u64) {
        self.conf = self.conf.take().map(|client| client.with_timeout(ticks));
    }

    /// Delete a profile's fields and, for a user's network, its secret (a
    /// system network's secret is `elevd`'s). Missing pieces are fine.
    pub fn erase(&mut self, profile: &Profile) {
        if let Some(client) = self.conf() {
            for field in profile::FIELDS.iter().rev() {
                let _ = client.delete(&profile::key(profile.owner, &profile.id, field));
            }
        }
        let (scope, uid) = scope_of(profile.owner);
        if profile.security == wlansta::Security::Wpa2Psk && profile.owner != Owner::System {
            if let Some(keyd) = self.keyd() {
                let _ = keyd.delete_secret_for(scope, &profile.id, uid);
            }
        }
    }

    /// The PMK of `owner`'s profile `id` for `ssid`, from `keyd`. Only "no
    /// such secret" (`ENOENT`) is the user's to fix; a refusal (`EPERM`,
    /// `EINVAL`: `keyd`'s policy or its own checks) or no answer is retried
    /// later (and a refusal printed), never taken for a wrong passphrase.
    pub fn pmk(&mut self, id: &str, owner: Owner, ssid: &[u8]) -> Result<Key<32>, PmkError> {
        let (scope, uid) = scope_of(owner);
        let Some(keyd) = self.keyd() else {
            return Err(PmkError::Unavailable);
        };
        match keyd.wifi_pmk(scope, id, ssid, uid) {
            Ok(mut bytes) => {
                let pmk = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| PmkError::Unavailable);
                user::messenger::wifi::wipe_bytes(&mut bytes);
                pmk.map(Key::new)
            }
            Err(error) => match error.errno().map(|code| -code) {
                Some(errno::ENOENT) => Err(PmkError::Missing),
                Some(code) if code == errno::EINVAL || code == errno::EPERM => {
                    trace::line!(trace::Channel::Wlanmd, "PMK:REFUSED {id} errno={code}");
                    Err(PmkError::Unavailable)
                }
                _ => {
                    self.keyd = None;
                    Err(PmkError::Unavailable)
                }
            },
        }
    }
}

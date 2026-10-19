//! An app's own secrets (docs/ai-agent-plan.md section 5.3 "P2b").
//!
//! A secret belongs to one `(uid, app)`: the caller's kernel-stamped uid and
//! the installed-app label it runs under (`app:<system_name>`), never a name in
//! a request. Unlike the named secrets of the crate root, an app can read its
//! own back ([`AppStore::get`]); nobody else can, because every call is keyed
//! by the caller's own identity ([`app_of_label`]): an unlabelled caller, a
//! development run (`dev:<name>`) and a helper process
//! (`app:<name>/<child>`) have no `app` and are refused.
//!
//! The table lives in its own versioned file (`app_file.rs`), beside the named
//! secrets' file, which stays as it is.

use alloc::string::String;
use alloc::vec::Vec;

/// Longest secret name.
pub const APP_MAX_NAME: usize = 64;
/// Longest secret: a password, a token or a refresh token.
pub const APP_MAX_SECRET: usize = 1024;
/// Secrets one app may hold for one user.
pub const APP_MAX_PER_APP: usize = 16;
/// Secrets in all, so a crowd of apps cannot fill the volume.
pub const APP_MAX_TOTAL: usize = 256;
/// Longest installed-app name (`system_name`) a secret is kept for.
pub const APP_MAX_ID: usize = 128;

const APP_PREFIX: &str = "app:";

/// Why an [`AppStore`] call failed. `keyd` maps these onto errno codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppError {
    /// The name breaks [`valid_app_name`].
    BadName,
    /// The secret breaks [`valid_app_secret`].
    BadSecret,
    /// The app or the table is at its cap.
    Full,
    /// No such secret.
    NotFound,
}

/// The installed app a task labelled `label` is, if it is one: `app:<id>`
/// with an id of 1..=[`APP_MAX_ID`] of letters, digits and `. _ -`. A
/// `dev:<id>` run and a helper (`app:<id>/<child>`) are not: they have other
/// labels, which is how a project under development or a sandboxed child
/// never sees the installed app's secrets.
pub fn app_of_label(label: &str) -> Option<&str> {
    let id = label.strip_prefix(APP_PREFIX)?;
    let ok = (1..=APP_MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    ok.then_some(id)
}

/// A name a secret may have: 1..=[`APP_MAX_NAME`] bytes of printable ASCII
/// (no space, no control character), so a name is safe to show in a list.
pub fn valid_app_name(name: &str) -> bool {
    (1..=APP_MAX_NAME).contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A secret `keyd` keeps for an app: 1..=[`APP_MAX_SECRET`] bytes.
pub fn valid_app_secret(secret: &[u8]) -> bool {
    (1..=APP_MAX_SECRET).contains(&secret.len())
}

#[derive(Clone)]
pub(crate) struct AppEntry {
    pub(crate) uid: u32,
    pub(crate) app: String,
    pub(crate) name: String,
    pub(crate) secret: Vec<u8>,
}

impl Drop for AppEntry {
    /// Scrub the secret on the way out (best effort: the allocator may have
    /// moved it before, and the compiler may not elide a volatile write).
    fn drop(&mut self) {
        for byte in self.secret.iter_mut() {
            // SAFETY: `byte` is a valid, aligned, exclusive reference.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
    }
}

/// The table of apps' secrets.
#[derive(Clone, Default)]
pub struct AppStore {
    pub(crate) entries: Vec<AppEntry>,
}

impl AppStore {
    /// An empty table.
    pub fn new() -> AppStore {
        AppStore::default()
    }

    /// How many secrets are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, uid: u32, app: &str, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.uid == uid && e.app == app && e.name == name)
    }

    /// Keep `secret` as `name` of `(uid, app)`, replacing one of that name.
    pub fn put(&mut self, uid: u32, app: &str, name: &str, secret: &[u8]) -> Result<(), AppError> {
        if !valid_app_name(name) {
            return Err(AppError::BadName);
        }
        if !valid_app_secret(secret) {
            return Err(AppError::BadSecret);
        }
        let entry = AppEntry {
            uid,
            app: String::from(app),
            name: String::from(name),
            secret: secret.to_vec(),
        };
        match self.position(uid, app, name) {
            Some(index) => self.entries[index] = entry,
            None => {
                let held = self
                    .entries
                    .iter()
                    .filter(|e| e.uid == uid && e.app == app)
                    .count();
                if held >= APP_MAX_PER_APP || self.entries.len() >= APP_MAX_TOTAL {
                    return Err(AppError::Full);
                }
                self.entries.push(entry);
            }
        }
        Ok(())
    }

    /// The secret `name` of `(uid, app)`.
    pub fn get(&self, uid: u32, app: &str, name: &str) -> Result<&[u8], AppError> {
        let index = self.position(uid, app, name).ok_or(AppError::NotFound)?;
        Ok(&self.entries[index].secret)
    }

    /// Forget the secret `name` of `(uid, app)`.
    pub fn delete(&mut self, uid: u32, app: &str, name: &str) -> Result<(), AppError> {
        let index = self.position(uid, app, name).ok_or(AppError::NotFound)?;
        self.entries.remove(index);
        Ok(())
    }

    /// The secret names of `(uid, app)`, sorted. Never the secrets.
    pub fn names(&self, uid: u32, app: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.uid == uid && e.app == app)
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The apps that hold a secret, each once, sorted.
    pub fn apps(&self) -> Vec<String> {
        let mut apps: Vec<String> = self.entries.iter().map(|e| e.app.clone()).collect();
        apps.sort();
        apps.dedup();
        apps
    }

    /// Delete every secret of the apps `installed` says are gone; returns how
    /// many went. It only ever deletes, and only for an app `installed`
    /// answers `false` for.
    pub fn sweep(&mut self, mut installed: impl FnMut(&str) -> bool) -> usize {
        let gone: Vec<String> = self
            .apps()
            .into_iter()
            .filter(|app| !installed(app))
            .collect();
        let before = self.entries.len();
        self.entries.retain(|e| !gone.contains(&e.app));
        before - self.entries.len()
    }
}

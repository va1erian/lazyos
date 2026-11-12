//! The `os.lazy.keyd.appsecret.v1` service half of `keyd`: who is calling, and
//! the four methods (`appsecrets.rs` is their table and disk).

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::server::{Caller, Fault};
use user::messenger::keyd::appsecret::wire;
use user::messenger::{self, errno, Error};
use user::sys;

use crate::appsecrets::AppSecrets;
use crate::{bounded_bytes, bounded_text};

/// Serve `os.lazy.keyd.appsecret.v1`: every call is for the caller's own
/// `(uid, installed-app label)`, resolved from its kernel-stamped credentials
/// ([`app_caller`]); no name in a request can reach another app's secrets.
pub(crate) struct Handler<'a> {
    pub(crate) entropy: &'a mut lazyos_crypto::rng::Entropy,
    pub(crate) secrets: &'a mut AppSecrets,
}

/// The caller's uid and the app id its label names; an unlabelled caller, or a
/// label that names no installed app, is refused.
fn app_of(cx: &Caller) -> messenger::Result<(u32, String)> {
    let (uid, label) = app_caller(cx)?;
    let app_id = secretstore::app_of_label(&label).ok_or(Error::Errno(-errno::EPERM))?;
    Ok((uid, String::from(app_id)))
}

impl wire::Server for Handler<'_> {
    fn put(&mut self, cx: &Caller, name: String, secret: Vec<u8>) -> Result<(), Fault> {
        let (uid, app_id) = app_of(cx)?;
        let name = bounded_text(name)?;
        let secret = bounded_bytes(secret, secretstore::APP_MAX_SECRET)?;
        Ok(self.secrets.put(self.entropy, uid, &app_id, &name, &secret)?)
    }

    fn get(&mut self, cx: &Caller, name: String) -> Result<Vec<u8>, Fault> {
        let (uid, app_id) = app_of(cx)?;
        Ok(self.secrets.get(uid, &app_id, &bounded_text(name)?)?)
    }

    fn delete(&mut self, cx: &Caller, name: String) -> Result<(), Fault> {
        let (uid, app_id) = app_of(cx)?;
        Ok(self.secrets.delete(self.entropy, uid, &app_id, &bounded_text(name)?)?)
    }

    fn list(&mut self, cx: &Caller) -> Result<Vec<String>, Fault> {
        let (uid, app_id) = app_of(cx)?;
        Ok(self.secrets.names(uid, &app_id))
    }
}

/// The caller's uid and installed-app label, from its kernel-stamped
/// credentials. An unlabelled caller (a system service) is
/// refused: it has no app, so nothing of its could be scoped. A label the
/// kernel cannot name is refused too, never guessed.
fn app_caller(cx: &Caller) -> messenger::Result<(u32, String)> {
    if cx.label_id == 0 {
        return Err(Error::Errno(-errno::EPERM));
    }
    let mut name = [0u8; sys::MAX_LABEL_BYTES];
    let len = sys::label_name(cx.label_id, &mut name).map_err(|_| Error::Errno(-errno::EPERM))?;
    let label = core::str::from_utf8(&name[..len]).map_err(|_| Error::Errno(-errno::EPERM))?;
    Ok((cx.uid, String::from(label)))
}

//! `pkgd`'s request dispatch: who is asking, the read-only methods, and the
//! framing of replies and failures. The state-changing flows (`Install`,
//! `Remove`, boot reconciliation) are in [`install`](super::install).
//!
//! Every method answers: a refusal is a structured error (errno-style code plus
//! friendly text, `messenger::pkgd::error_reply`), never silence, so a caller
//! never waits on a request that was dropped.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use pkgstore::access::{self, Caller};
use pkgstore::layout;
use user::messenger::pkgd::{self, wire, Failure};
use user::messenger::{accounts, Message, Parcel};
use user::sys;

use super::audit::Audit;
use super::inspect::assess;
use super::peers::Peer;
use super::registry::Registry;
use super::store;

pub(crate) const EPERM: i64 = 1;
pub(crate) const ENOENT: i64 = 2;
pub(crate) const EIO: i64 = 5;
pub(crate) const ENODEV: i64 = 19;
pub(crate) const EEXIST: i64 = 17;
pub(crate) const EINVAL: i64 = 22;

/// A refusal or failure with its code and text.
pub(crate) fn fail(code: i64, text: impl Into<String>) -> Failure {
    Failure {
        code,
        text: text.into(),
    }
}

/// The service state shared by every request.
pub(crate) struct Pkgd {
    pub(crate) registry: Registry,
    pub(crate) audit: Audit,
    /// The account database, for a caller's home directory.
    accounts: Peer,
    /// The package file buffer, reused so a service that reads many packages
    /// does not grow by one package each time.
    pub(crate) buffer: Vec<u8>,
}

impl Pkgd {
    pub(crate) const fn new() -> Pkgd {
        Pkgd {
            registry: Registry::new(),
            audit: Audit::new(),
            accounts: Peer::new(accounts::NAME),
            buffer: Vec::new(),
        }
    }

    /// Answer one request.
    pub(crate) fn dispatch(&mut self, message: &Message) -> Parcel {
        let method = message.method();
        if message.interface_id() != pkgd::INTERFACE {
            return pkgd::error_reply(method, EINVAL, "that is not a package manager request");
        }
        let Some(caller) = caller_of(message) else {
            return pkgd::error_reply(method, EPERM, "the caller's identity could not be read");
        };
        match self.handle(method, &caller, &message.parcel.body) {
            Ok(body) => pkgd::parcel(method, body),
            Err(failure) => pkgd::error_reply(method, failure.code, &failure.text),
        }
    }

    fn handle(&mut self, method: u32, caller: &Caller, body: &[u8]) -> Result<Vec<u8>, Failure> {
        let malformed = |_| fail(EINVAL, "the request is malformed");
        match method {
            wire::METHOD_INSPECT => {
                let args = wire::decode_inspect_args(body).map_err(malformed)?;
                let info = self.inspect(caller, &args.path)?;
                wire::encode_inspect_reply(&wire::InspectReply { info }).map_err(malformed)
            }
            wire::METHOD_INSTALL => {
                let args = wire::decode_install_args(body).map_err(malformed)?;
                let app = self.install(caller, &args.path)?;
                wire::encode_install_reply(&wire::InstallReply { app }).map_err(malformed)
            }
            wire::METHOD_REMOVE => {
                let args = wire::decode_remove_args(body).map_err(malformed)?;
                self.remove(caller, &args.system_name)?;
                Ok(Vec::new())
            }
            wire::METHOD_LIST => {
                access::may_inspect(caller).map_err(|why| fail(EPERM, why))?;
                let apps = self.registry.list().map_err(registry_down)?;
                wire::encode_list_reply(&wire::ListReply { apps }).map_err(malformed)
            }
            wire::METHOD_INSTALLED => {
                let args = wire::decode_installed_args(body).map_err(malformed)?;
                access::may_inspect(caller).map_err(|why| fail(EPERM, why))?;
                if !layout::valid_system_name(&args.system_name) {
                    return Err(fail(EINVAL, "that is not a valid application name"));
                }
                let app = self
                    .registry
                    .get(&args.system_name)
                    .map_err(registry_down)?;
                wire::encode_installed_reply(&wire::InstalledReply { app }).map_err(malformed)
            }
            _ => Err(fail(EINVAL, "that is not a package manager method")),
        }
    }

    /// `Inspect(path)`: read and validate the package, change nothing.
    fn inspect(&mut self, caller: &Caller, path: &str) -> Result<wire::PackageInfo, Failure> {
        access::may_inspect(caller).map_err(|why| fail(EPERM, why))?;
        let path = self.check_source(caller, path)?;
        store::read_package(&mut self.buffer, &path).map_err(read_failure)?;
        let bytes = core::mem::take(&mut self.buffer);
        let info = match assess(&bytes) {
            Ok(assessed) => assessed.info,
            Err(info) => *info,
        };
        self.buffer = bytes;
        Ok(info)
    }

    /// Whether `pkgd` will read `path` for `caller` (see `pkgstore::access`),
    /// and the normalised path to read.
    pub(crate) fn check_source(&mut self, caller: &Caller, path: &str) -> Result<String, Failure> {
        // Root may read anywhere; anyone else may also use their own home.
        let home = if caller.uid == 0 {
            None
        } else {
            self.home_of(caller.uid)
        };
        access::source_allowed(caller, home.as_deref(), path).map_err(|why| fail(EPERM, why))
    }

    /// `uid`'s home directory from the account database, when it answers.
    fn home_of(&mut self, uid: u32) -> Option<String> {
        let record = self
            .accounts
            .run(|endpoint| accounts::lookup_uid(&endpoint, uid))
            .ok()??;
        Some(record.home)
    }
}

/// The kernel-stamped identity of the sender. `pkgd` is root, so it may read
/// another task's credential block.
fn caller_of(message: &Message) -> Option<Caller> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).ok()?;
    Some(Caller {
        uid: cred.uid,
        session: cred.session,
        label_id: cred.label_id,
    })
}

/// A package file that could not be read.
pub(crate) fn read_failure(code: i64) -> Failure {
    match code {
        2 => fail(ENOENT, "that file does not exist"),
        13 => fail(EPERM, "that file cannot be read"),
        21 => fail(EINVAL, "that is a folder, not a package"),
        27 => fail(
            EINVAL,
            format!(
                "that file is larger than the {} MiB package limit",
                store::MAX_PACKAGE_FILE / (1024 * 1024)
            ),
        ),
        other => fail(
            EIO,
            format!(
                "that file could not be read: {}",
                user::files::describe(other)
            ),
        ),
    }
}

/// The configuration registry (where installed apps are recorded) is not
/// answering.
pub(crate) fn registry_down(error: user::messenger::Error) -> Failure {
    fail(
        EIO,
        format!(
            "the list of installed applications is unavailable: {}",
            error.message()
        ),
    )
}

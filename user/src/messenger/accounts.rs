//! The account database's client and server shapes (issue #101 companion).
//!
//! The `os.lazy.accounts.v1` stubs and the blocking [`Client`] are generated
//! by `midlc`; this module adds the lookups that return a record or `None`
//! ([`Directory`]).

use alloc::string::String;

use libmessenger::client::{Deadline, Transport};

use super::client::Native;

/// The generated `os.lazy.accounts.v1` stubs (`idl/accounts.midl`).
pub use messenger_generated::os_lazy_accounts_v1 as wire;

/// One account record, as a lookup reply carries it.
pub use wire::User as UserRecord;

/// The accounts service's registered name.
pub const NAME: &str = wire::SERVICE_NAME;

/// The interface id every accounts parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `accountsd`: `Client::try_connect(Native)`.
pub type Client = wire::Client<Native>;

/// Why an `accountsd` call failed (the service declares no error codes; a
/// refusal is its errno-style code and sentence).
pub type ClientError = wire::ClientError;

/// Lookups by name or uid that answer the record, or `None` when no such
/// account exists.
pub trait Directory {
    /// The account called `name`.
    fn lookup_name(&self, name: &str) -> core::result::Result<Option<UserRecord>, ClientError>;
    /// The account of `uid`.
    fn lookup_uid(&self, uid: u32) -> core::result::Result<Option<UserRecord>, ClientError>;
    /// [`Directory::lookup_uid`] that gives up after `within`.
    fn lookup_uid_within(
        &self,
        uid: u32,
        within: Deadline,
    ) -> core::result::Result<Option<UserRecord>, ClientError>;
}

fn record(reply: wire::LookupReply) -> Option<UserRecord> {
    if reply.found {
        reply.user
    } else {
        None
    }
}

impl<T: Transport> Directory for wire::Client<T> {
    fn lookup_name(&self, name: &str) -> core::result::Result<Option<UserRecord>, ClientError> {
        self.lookup(Some(name), None).map(record)
    }

    fn lookup_uid(&self, uid: u32) -> core::result::Result<Option<UserRecord>, ClientError> {
        self.lookup(None, Some(uid)).map(record)
    }

    fn lookup_uid_within(
        &self,
        uid: u32,
        within: Deadline,
    ) -> core::result::Result<Option<UserRecord>, ClientError> {
        self.lookup_within(None, Some(uid), within).map(record)
    }
}

/// The environment of a session's programs (issue #508): `HOME` and `USER`
/// from the account, and `PATH` naming the one program directory. `logind`
/// gives it to the console shell and `init` to every app it launches into a
/// session, so both agree on it.
pub fn session_env(name: &str, home: &str) -> [String; 3] {
    [
        alloc::format!("HOME={home}"),
        alloc::format!("USER={name}"),
        alloc::format!("PATH={}", fhs::SYSTEM_BIN),
    ]
}

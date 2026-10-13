//! `mimed`'s per-user choices of default app (docs/lazywebplus.md): when two installed apps handle
//! a type (LazyWeb and LazyWeb+ both open `x-scheme-handler/https`), the registry alone lets
//! whichever registered last win, which is install order. A user who has an opinion keeps it in
//! `confd`, in their own subtree, under the key `deskmenu::defaults::user_key` builds (the one
//! definition of it, shared with Settings, which writes it):
//!
//! ```text
//! user/<uid>/mimed/default/<mime type>      value: the app id, e.g. os.lazy.lazywebplus
//! ```
//!
//! `Open` reads it for the caller's uid and uses it **only** when that app still holds a
//! registration for the pair (the app is installed and handles the type; a stale or invented id is
//! ignored). The key is the user's own (`confd` lets only that uid and system services touch
//! `user/<uid>/**`), so one account's choice never moves another's.
//!
//! The `confd` client is resolved on first use and kept for the life of the service (a resolved
//! endpoint must never be closed, `user::messenger`), with a short timeout: an unreachable `confd`
//! means "no preference", never a stalled `Open`.

use alloc::string::String;
use deskmenu::defaults;
use user::messenger::confd::Client;

/// How long one `confd` read may take, in PIT ticks (100 Hz): a fifth of a second.
const READ_TICKS: u64 = 20;

/// The users' choices, read from `confd` when asked.
pub(crate) struct Prefs {
    client: Option<Client>,
}

impl Prefs {
    pub(crate) const fn new() -> Prefs {
        Prefs { client: None }
    }

    /// The app user `uid` chose for `mime`, if they chose one and `confd` answers.
    pub(crate) fn default_app(&mut self, uid: u32, mime: &str) -> Option<String> {
        let path = defaults::user_key(uid, mime)?;
        if self.client.is_none() {
            self.client = Client::connect()
                .ok()
                .map(|client| client.with_timeout(READ_TICKS));
        }
        let client = self.client.as_ref()?;
        match client.get(&path) {
            Ok(value) => defaults::app(value.as_ref()).map(String::from),
            Err(_) => {
                // A service that restarted has a new endpoint: resolve again next time (an
                // endpoint is a plain handle number, dropping it closes nothing).
                self.client = None;
                None
            }
        }
    }
}

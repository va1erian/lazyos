//! Where Mail keeps account passwords: in this process's memory only.
//!
//! esMail reads and writes passwords through the `keyring` crate. LazyOS has no
//! secrets service yet (docs/tls-plan.md §6.3), so this installs a keyring
//! backend that holds them for the session and forgets them at exit: the
//! "prompted, never stored" first cut. The account itself (servers, user name)
//! is saved in `config.toml` as usual, so the next session only asks for the
//! password again.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi};

type Store = Arc<Mutex<HashMap<(String, String), Vec<u8>>>>;

/// Makes the session store the backend of every `keyring::Entry`.
pub fn install() {
    keyring::set_default_credential_builder(Box::new(SessionBuilder {
        store: Store::default(),
    }));
}

/// Hands out entries over one shared map, so an entry built for a read sees
/// what another one wrote.
struct SessionBuilder {
    store: Store,
}

impl CredentialBuilderApi for SessionBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<Credential>> {
        let key = (service.to_owned(), user.to_owned());
        Ok(Box::new(SessionEntry {
            store: Arc::clone(&self.store),
            key,
        }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SessionEntry {
    store: Store,
    key: (String, String),
}

impl SessionEntry {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<(String, String), Vec<u8>>) -> R) -> R {
        f(&mut self.store.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl CredentialApi for SessionEntry {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        self.with(|map| map.insert(self.key.clone(), secret.to_vec()));
        Ok(())
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        self.with(|map| map.get(&self.key).cloned())
            .ok_or(keyring::Error::NoEntry)
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        self.with(|map| map.remove(&self.key))
            .map(drop)
            .ok_or(keyring::Error::NoEntry)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    // The default prints the secret's container; never show it.
    fn debug_fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionEntry({}:{})", self.key.0, self.key.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(builder: &SessionBuilder, user: &str) -> Box<Credential> {
        builder.build(None, "esmail", user).unwrap()
    }

    #[test]
    fn a_secret_is_shared_between_entries_and_forgotten_on_delete() {
        let builder = SessionBuilder {
            store: Store::default(),
        };
        entry(&builder, "a:imap").set_password("hunter2").unwrap();
        assert_eq!(entry(&builder, "a:imap").get_password().unwrap(), "hunter2");
        assert!(matches!(
            entry(&builder, "a:smtp").get_password(),
            Err(keyring::Error::NoEntry)
        ));
        entry(&builder, "a:imap").delete_credential().unwrap();
        assert!(matches!(
            entry(&builder, "a:imap").get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn debug_output_never_contains_the_secret() {
        let builder = SessionBuilder {
            store: Store::default(),
        };
        let credential = entry(&builder, "a:imap");
        credential.set_password("hunter2").unwrap();
        let mut text = String::new();
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{credential:?}"));
        assert!(!text.contains("hunter2"));
    }
}

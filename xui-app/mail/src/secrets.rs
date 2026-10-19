//! Where Mail keeps account passwords: in this process's memory, and in
//! `keyd` when the user ticked "Remember this password".
//!
//! esMail reads and writes passwords through the `keyring` crate. This
//! installs a keyring backend with two layers:
//!
//! * **the session**: a map in this process's memory, forgotten at exit. It is
//!   always written, so a password typed once is not asked for again while
//!   Mail runs;
//! * **the vault**: `keyd`'s app secrets (`os.lazy.keyd.appsecret.v1`,
//!   docs/ai-agent-plan.md section 5.3), sealed on disk and scoped to this app
//!   and user, written only while "Remember this password" is ticked
//!   ([`set_remember`]) and read when the session has none, which is how a
//!   remembered password survives a reboot. Saving with it unticked deletes
//!   the stored copy.
//!
//! The account itself (servers, user name) is saved in `config.toml` as usual.
//! A vault that cannot be reached (no permission, no `keyd`) never fails the
//! save: the password stays in the session and Mail asks again next time.
//! Nothing here prints or formats a password; the vault's name for an entry is
//! a hash of it, never the address.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi};
use sha2::{Digest, Sha256};
use xui_app::platform::appsecret::{self, Failure};

type Key = (String, String);
type Store = Arc<Mutex<HashMap<Key, Vec<u8>>>>;

/// Whether the next password written is kept in the vault too.
static REMEMBER: AtomicBool = AtomicBool::new(false);

/// The account form's "Remember this password" checkbox, read when it is
/// saved.
pub fn set_remember(on: bool) {
    REMEMBER.store(on, Ordering::Relaxed);
}

/// Where a remembered password lives. `keyd` is the real one; a test brings
/// its own.
pub trait Vault: Send + Sync {
    fn put(&self, name: &str, secret: &[u8]) -> Result<(), Failure>;
    fn get(&self, name: &str) -> Result<Vec<u8>, Failure>;
    fn delete(&self, name: &str) -> Result<(), Failure>;
}

/// `keyd`'s app secrets.
struct Keyd;

impl Vault for Keyd {
    fn put(&self, name: &str, secret: &[u8]) -> Result<(), Failure> {
        appsecret::put(name, secret)
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, Failure> {
        appsecret::get(name)
    }

    fn delete(&self, name: &str) -> Result<(), Failure> {
        appsecret::delete(name)
    }
}

/// Makes the session store, backed by `keyd`, the backend of every
/// `keyring::Entry`.
pub fn install() {
    keyring::set_default_credential_builder(Box::new(SessionBuilder {
        store: Store::default(),
        vault: Arc::new(Keyd),
        remember: &REMEMBER,
    }));
}

/// The vault's name of an entry: `mail:` and 24 bytes of a SHA-256 of the
/// service and user, in hex (53 printable ASCII characters, under `keyd`'s
/// limit whatever the address is).
fn vault_name(key: &Key) -> String {
    let mut hash = Sha256::new();
    hash.update(key.0.as_bytes());
    hash.update([0]);
    hash.update(key.1.as_bytes());
    let digest = hash.finalize();
    let mut name = String::from("mail:");
    for byte in &digest[..24] {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// Hands out entries over one shared map, so an entry built for a read sees
/// what another one wrote.
struct SessionBuilder {
    store: Store,
    vault: Arc<dyn Vault>,
    remember: &'static AtomicBool,
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
            vault: Arc::clone(&self.vault),
            remember: self.remember,
            key,
        }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SessionEntry {
    store: Store,
    vault: Arc<dyn Vault>,
    remember: &'static AtomicBool,
    key: Key,
}

impl SessionEntry {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<Key, Vec<u8>>) -> R) -> R {
        f(&mut self.store.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl CredentialApi for SessionEntry {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        self.with(|map| map.insert(self.key.clone(), secret.to_vec()));
        let name = vault_name(&self.key);
        // Ticked: keep it. Unticked: forget what an earlier save kept. A
        // vault that fails does not fail the save; the session has it.
        if self.remember.load(Ordering::Relaxed) {
            let _ = self.vault.put(&name, secret);
        } else {
            let _ = self.vault.delete(&name);
        }
        Ok(())
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        if let Some(secret) = self.with(|map| map.get(&self.key).cloned()) {
            return Ok(secret);
        }
        // Not typed this session: the one a past session remembered.
        let secret = self
            .vault
            .get(&vault_name(&self.key))
            .map_err(|_| keyring::Error::NoEntry)?;
        self.with(|map| map.insert(self.key.clone(), secret.clone()));
        Ok(secret)
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        let session = self.with(|map| map.remove(&self.key)).is_some();
        let stored = self.vault.delete(&vault_name(&self.key)).is_ok();
        if session || stored {
            Ok(())
        } else {
            Err(keyring::Error::NoEntry)
        }
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

    /// A vault in memory, with a switch that makes it fail.
    #[derive(Default)]
    struct Fake {
        map: Mutex<HashMap<String, Vec<u8>>>,
        broken: AtomicBool,
    }

    impl Fake {
        fn check(&self) -> Result<(), Failure> {
            if self.broken.load(Ordering::Relaxed) {
                Err(Failure::Unavailable)
            } else {
                Ok(())
            }
        }
    }

    impl Vault for Fake {
        fn put(&self, name: &str, secret: &[u8]) -> Result<(), Failure> {
            self.check()?;
            self.map
                .lock()
                .unwrap()
                .insert(name.to_owned(), secret.to_vec());
            Ok(())
        }

        fn get(&self, name: &str) -> Result<Vec<u8>, Failure> {
            self.check()?;
            self.map
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or(Failure::NotFound)
        }

        fn delete(&self, name: &str) -> Result<(), Failure> {
            self.check()?;
            self.map
                .lock()
                .unwrap()
                .remove(name)
                .map(drop)
                .ok_or(Failure::NotFound)
        }
    }

    static ON: AtomicBool = AtomicBool::new(true);
    static OFF: AtomicBool = AtomicBool::new(false);

    fn builder(vault: &Arc<Fake>, remember: &'static AtomicBool) -> SessionBuilder {
        SessionBuilder {
            store: Store::default(),
            vault: vault.clone(),
            remember,
        }
    }

    fn entry(builder: &SessionBuilder, user: &str) -> Box<Credential> {
        builder.build(None, "esmail", user).unwrap()
    }

    #[test]
    fn a_secret_is_shared_between_entries_and_forgotten_on_delete() {
        let vault = Arc::new(Fake::default());
        let builder = builder(&vault, &OFF);
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
    fn nothing_reaches_the_vault_unless_remember_is_on() {
        let vault = Arc::new(Fake::default());
        entry(&builder(&vault, &OFF), "a:imap")
            .set_password("hunter2")
            .unwrap();
        assert!(vault.map.lock().unwrap().is_empty());
    }

    #[test]
    fn a_remembered_password_survives_a_restart() {
        let vault = Arc::new(Fake::default());
        entry(&builder(&vault, &ON), "a:imap")
            .set_password("hunter2")
            .unwrap();
        assert_eq!(vault.map.lock().unwrap().len(), 1);
        // A new process: an empty session over the same vault.
        let fresh = builder(&vault, &OFF);
        assert_eq!(entry(&fresh, "a:imap").get_password().unwrap(), "hunter2");
        assert!(matches!(
            entry(&fresh, "b:imap").get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn saving_without_remember_forgets_the_stored_copy() {
        let vault = Arc::new(Fake::default());
        entry(&builder(&vault, &ON), "a:imap")
            .set_password("hunter2")
            .unwrap();
        entry(&builder(&vault, &OFF), "a:imap")
            .set_password("hunter3")
            .unwrap();
        assert!(vault.map.lock().unwrap().is_empty());
        assert!(matches!(
            entry(&builder(&vault, &OFF), "a:imap").get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn deleting_removes_the_stored_copy_too() {
        let vault = Arc::new(Fake::default());
        let session = builder(&vault, &ON);
        entry(&session, "a:imap").set_password("hunter2").unwrap();
        entry(&session, "a:imap").delete_credential().unwrap();
        assert!(vault.map.lock().unwrap().is_empty());
    }

    #[test]
    fn a_broken_vault_never_fails_the_save() {
        let vault = Arc::new(Fake::default());
        vault.broken.store(true, Ordering::Relaxed);
        let session = builder(&vault, &ON);
        entry(&session, "a:imap").set_password("hunter2").unwrap();
        // The session still has it; only the next run will have to ask.
        assert_eq!(entry(&session, "a:imap").get_password().unwrap(), "hunter2");
        assert!(matches!(
            entry(&builder(&vault, &ON), "a:imap").get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn the_vault_name_is_short_printable_and_hides_the_address() {
        let long = "x".repeat(400);
        for user in ["a@b.c:imap", long.as_str(), "caf\u{e9}@ex.org:smtp"] {
            let name = vault_name(&("esmail".to_owned(), user.to_owned()));
            assert_eq!(name.len(), 53);
            assert!(name.bytes().all(|b| b.is_ascii_graphic()), "{name}");
            assert!(!name.contains("imap") && !name.contains('@'));
        }
        let one = vault_name(&("esmail".to_owned(), "a:imap".to_owned()));
        assert_eq!(one, vault_name(&("esmail".to_owned(), "a:imap".to_owned())));
        assert_ne!(one, vault_name(&("esmail".to_owned(), "a:smtp".to_owned())));
        assert_ne!(one, vault_name(&("esmai".to_owned(), "la:imap".to_owned())));
    }

    #[test]
    fn debug_output_never_contains_the_secret() {
        let vault = Arc::new(Fake::default());
        let builder = builder(&vault, &OFF);
        let credential = entry(&builder, "a:imap");
        credential.set_password("hunter2").unwrap();
        let mut text = String::new();
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{credential:?}"));
        assert!(!text.contains("hunter2"));
    }
}

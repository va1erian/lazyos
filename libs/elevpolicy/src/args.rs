//! [`Operation::args`]: an operation back to the arguments
//! [`Operation::parse`] reads, for the wire and the round-trip tests.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{value_args, Operation, WifiChange};

impl Operation {
    /// The arguments [`Operation::parse`] reads back.
    pub fn args(&self) -> Vec<String> {
        let one = |text: &str| alloc::vec![text.to_string()];
        match self {
            Operation::PkgInstall { path } | Operation::PkgUpdateCore { path } => one(path),
            Operation::PkgRemove { system_name } => one(system_name),
            Operation::ConfSet { path, value } => {
                let (kind, text) = value_args(value);
                alloc::vec![path.clone(), kind.to_string(), text]
            }
            Operation::ConfDelete { path } | Operation::ConfGet { path } => one(path),
            Operation::ConfList { prefix } => one(prefix),
            Operation::ConfElevate => Vec::new(),
            Operation::TimeSet { unix } => alloc::vec![format!("{unix}")],
            Operation::AccountCreate {
                name,
                secret,
                admin,
            } => alloc::vec![
                name.clone(),
                secret.clone(),
                String::from(if *admin { "admin" } else { "user" })
            ],
            Operation::AccountDelete { name, home } => {
                alloc::vec![name.clone(), home.word().to_string()]
            }
            Operation::AccountAdmin { name, admin } => {
                alloc::vec![name.clone(), String::from(if *admin { "1" } else { "0" })]
            }
            Operation::AccountPassword { name, secret } => {
                alloc::vec![name.clone(), secret.clone()]
            }
            Operation::PowerPolicy { key, value } => alloc::vec![key.clone(), value.clone()],
            Operation::NetConfig {
                card,
                address,
                gateway,
                dns,
            } => alloc::vec![card.clone(), address.clone(), gateway.clone(), dns.clone()],
            Operation::ServiceRestart { name } => one(name),
            Operation::WifiSystem(WifiChange::Store { name, passphrase }) => {
                alloc::vec![String::from("store"), name.clone(), passphrase.clone()]
            }
            Operation::WifiSystem(WifiChange::Delete { name }) => {
                alloc::vec![String::from("delete"), name.clone()]
            }
        }
    }
}

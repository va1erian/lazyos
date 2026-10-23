//! Who is calling, as the kernel stamped it, and the web process group each caller's views share.
//! Nothing here comes from a request body: the identity is the sender's, and a view belongs to the
//! [`ClientKey`] that made it for as long as it lives.

use std::collections::BTreeMap;

use crate::limits::PROCESS_KEYS_PER_CLIENT;

/// The sender of a message as the kernel stamped it when the message was queued
/// (`SenderId` plus the task slot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Origin {
    pub uid: u32,
    /// The package label (`app:<system_name>`) as a number; 0 for no label (a script run from the
    /// Terminal).
    pub label: u32,
    pub session: u64,
    /// The sender's task slot. Only used to tell unlabelled callers apart.
    pub sender: u64,
}

/// What a view belongs to. Two callers with the same key are the same client: the same user running
/// the same package, from any of its threads or processes. An unlabelled caller has no package to
/// share with, so it is told apart by task and never shares a view or a quota with another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientKey {
    pub uid: u32,
    pub label: u32,
    pub task: u64,
}

impl Origin {
    pub fn client(&self) -> ClientKey {
        ClientKey {
            uid: self.uid,
            label: self.label,
            task: if self.label == 0 { self.sender } else { 0 },
        }
    }
}

/// Why [`Groups`] made no group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupError {
    /// Every number is taken.
    NoNumber,
    /// The client already holds [`PROCESS_KEYS_PER_CLIENT`] keys.
    TooManyKeys,
}

/// The `group` numbers the service passes to the engine: views with the same non-zero group share one
/// web process, and nothing but the service chooses them. A group is a client and a *process key*, the
/// placement hint of `ViewConfig.process_key` (`""`: the client's default group). A number belongs to
/// one (client, key) for good and is never given to another, so a client's views with one key always
/// share a process and two clients' views never do; a key can only split its own client's views further.
/// The default group of a labelled client is kept for good. A keyed group is released with its last
/// view ([`Groups::release`]), and an unlabelled client loses all of its groups with its last view, to
/// keep the table from growing with every host a browser visited or short-lived script; a number is
/// still never reused.
#[derive(Debug)]
pub struct Groups {
    by_key: BTreeMap<(ClientKey, String), i32>,
    /// The next number to hand out; `None` once `i32::MAX` is taken.
    next: Option<i32>,
}

impl Default for Groups {
    fn default() -> Self {
        Groups {
            by_key: BTreeMap::new(),
            next: Some(1),
        }
    }
}

impl Groups {
    /// The client's default group, made on first use; `None` once the numbers are used up.
    pub fn group_of(&mut self, client: ClientKey) -> Option<i32> {
        self.group_for(client, "").ok()
    }

    /// The group of `client`'s views with process key `key`, made on first use. A new non-empty key
    /// past [`PROCESS_KEYS_PER_CLIENT`] is refused; a key the client holds already always succeeds.
    pub fn group_for(&mut self, client: ClientKey, key: &str) -> Result<i32, GroupError> {
        if let Some(group) = self.by_key.get(&(client, key.to_owned())) {
            return Ok(*group);
        }
        if !key.is_empty() && self.keys_of(client) >= PROCESS_KEYS_PER_CLIENT {
            return Err(GroupError::TooManyKeys);
        }
        let group = self.next.ok_or(GroupError::NoNumber)?;
        self.next = group.checked_add(1);
        self.by_key.insert((client, key.to_owned()), group);
        Ok(group)
    }

    /// The non-empty keys `client` holds.
    pub fn keys_of(&self, client: ClientKey) -> usize {
        self.by_key
            .keys()
            .filter(|(owner, key)| *owner == client && !key.is_empty())
            .count()
    }

    /// The last view of `client` with process key `key` ended: a keyed group goes (the default group
    /// stays for a labelled client, as before).
    pub fn release(&mut self, client: ClientKey, key: &str) {
        if !key.is_empty() {
            self.by_key.remove(&(client, key.to_owned()));
        }
    }

    /// The client has no view left: an unlabelled client's entries go.
    pub fn idle(&mut self, client: ClientKey) {
        if client.label == 0 {
            self.by_key.retain(|(owner, _), _| *owner != client);
        }
    }

    /// The default group already made for `client`, if any.
    pub fn get(&self, client: ClientKey) -> Option<i32> {
        self.by_key.get(&(client, String::new())).copied()
    }

    /// The group already made for `client` and `key`, if any.
    pub fn get_keyed(&self, client: ClientKey, key: &str) -> Option<i32> {
        self.by_key.get(&(client, key.to_owned())).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(uid: u32, label: u32, sender: u64) -> Origin {
        Origin {
            uid,
            label,
            session: 1,
            sender,
        }
    }

    #[test]
    fn a_labelled_client_is_one_client_whatever_task_it_calls_from() {
        assert_eq!(origin(1000, 7, 3).client(), origin(1000, 7, 99).client());
        assert_ne!(origin(1000, 7, 3).client(), origin(1000, 8, 3).client());
        assert_ne!(origin(1000, 7, 3).client(), origin(1001, 7, 3).client());
    }

    #[test]
    fn unlabelled_callers_are_told_apart_by_task() {
        assert_ne!(origin(1000, 0, 3).client(), origin(1000, 0, 4).client());
        assert_eq!(origin(1000, 0, 3).client(), origin(1000, 0, 3).client());
    }

    #[test]
    fn groups_are_stable_per_client_and_never_shared() {
        let mut groups = Groups::default();
        let a = origin(1000, 7, 1).client();
        let b = origin(1000, 8, 2).client();
        let first = groups.group_of(a).unwrap();
        assert!(first > 0);
        assert_eq!(groups.group_of(a), Some(first));
        assert_ne!(groups.group_of(b), Some(first));
        groups.idle(a);
        assert_eq!(
            groups.get(a),
            Some(first),
            "a labelled client keeps its number"
        );
    }

    #[test]
    fn an_unlabelled_client_forgets_its_group_but_the_number_is_not_reused() {
        let mut groups = Groups::default();
        let script = origin(1000, 0, 5).client();
        let first = groups.group_of(script).unwrap();
        groups.idle(script);
        assert_eq!(groups.get(script), None);
        assert_ne!(groups.group_of(script), Some(first));
    }

    #[test]
    fn the_numbers_run_out_cleanly() {
        let mut groups = Groups {
            by_key: BTreeMap::new(),
            next: Some(i32::MAX),
        };
        assert_eq!(groups.group_of(origin(1, 1, 1).client()), Some(i32::MAX));
        assert_eq!(groups.group_of(origin(1, 2, 1).client()), None);
    }

    #[test]
    fn a_key_splits_a_clients_group_and_never_joins_another_clients() {
        let mut groups = Groups::default();
        let a = origin(1000, 7, 1).client();
        let b = origin(1000, 8, 2).client();
        let default = groups.group_for(a, "").unwrap();
        let site = groups.group_for(a, "example.com").unwrap();
        assert_ne!(default, site);
        assert_eq!(groups.group_for(a, "example.com"), Ok(site));
        // The same key of another client is another group: a key is a hint inside the caller's own group.
        let theirs = groups.group_for(b, "example.com").unwrap();
        assert_ne!(theirs, site);
        assert_ne!(theirs, default);
    }

    #[test]
    fn a_keyed_group_is_released_and_its_number_not_reused() {
        let mut groups = Groups::default();
        let a = origin(1000, 7, 1).client();
        let first = groups.group_for(a, "one.test").unwrap();
        groups.release(a, "one.test");
        assert_eq!(groups.get_keyed(a, "one.test"), None);
        assert_eq!(groups.keys_of(a), 0);
        assert_ne!(groups.group_for(a, "one.test"), Ok(first));
    }

    #[test]
    fn the_default_group_is_not_released_for_a_labelled_client() {
        let mut groups = Groups::default();
        let a = origin(1000, 7, 1).client();
        let default = groups.group_of(a).unwrap();
        groups.release(a, "");
        assert_eq!(groups.get(a), Some(default));
    }

    #[test]
    fn a_client_holds_a_bounded_number_of_keys() {
        let mut groups = Groups::default();
        let a = origin(1000, 7, 1).client();
        let b = origin(1000, 8, 1).client();
        for n in 0..PROCESS_KEYS_PER_CLIENT {
            groups.group_for(a, &format!("site{n}.test")).unwrap();
        }
        assert_eq!(
            groups.group_for(a, "one-more.test"),
            Err(GroupError::TooManyKeys)
        );
        // A key held already, the default group and another client are not affected.
        assert!(groups.group_for(a, "site0.test").is_ok());
        assert!(groups.group_for(a, "").is_ok());
        assert!(groups.group_for(b, "one-more.test").is_ok());
        groups.release(a, "site3.test");
        assert!(groups.group_for(a, "one-more.test").is_ok());
    }

    #[test]
    fn an_unlabelled_client_loses_every_group_with_its_last_view() {
        let mut groups = Groups::default();
        let script = origin(1000, 0, 5).client();
        groups.group_for(script, "a.test").unwrap();
        groups.group_of(script).unwrap();
        groups.idle(script);
        assert_eq!(groups.get(script), None);
        assert_eq!(groups.keys_of(script), 0);
    }
}

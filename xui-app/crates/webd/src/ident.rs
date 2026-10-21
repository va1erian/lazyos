//! Who is calling, as the kernel stamped it, and the web process group each caller's views share.
//! Nothing here comes from a request body: the identity is the sender's, and a view belongs to the
//! [`ClientKey`] that made it for as long as it lives.

use std::collections::BTreeMap;

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

/// The `group` numbers the service passes to the engine: views with the same non-zero group share one
/// web process, and nothing but the service chooses them. A number belongs to one client for
/// good and is never given to another, so a client's views always share a process and two clients'
/// views never do. (Unlabelled callers lose their number with their last view, to keep the table
/// from growing with every short-lived script; the number is still never reused.)
#[derive(Debug)]
pub struct Groups {
    by_client: BTreeMap<ClientKey, i32>,
    /// The next number to hand out; `None` once `i32::MAX` is taken.
    next: Option<i32>,
}

impl Default for Groups {
    fn default() -> Self {
        Groups {
            by_client: BTreeMap::new(),
            next: Some(1),
        }
    }
}

impl Groups {
    /// The client's group, made on first use; `None` once the numbers are used up.
    pub fn group_of(&mut self, client: ClientKey) -> Option<i32> {
        if let Some(group) = self.by_client.get(&client) {
            return Some(*group);
        }
        let group = self.next?;
        self.next = group.checked_add(1);
        self.by_client.insert(client, group);
        Some(group)
    }

    /// The client has no view left: an unlabelled client's entry goes.
    pub fn idle(&mut self, client: ClientKey) {
        if client.label == 0 {
            self.by_client.remove(&client);
        }
    }

    /// The group already made for `client`, if any.
    pub fn get(&self, client: ClientKey) -> Option<i32> {
        self.by_client.get(&client).copied()
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
            by_client: BTreeMap::new(),
            next: Some(i32::MAX),
        };
        assert_eq!(groups.group_of(origin(1, 1, 1).client()), Some(i32::MAX));
        assert_eq!(groups.group_of(origin(1, 2, 1).client()), None);
    }
}

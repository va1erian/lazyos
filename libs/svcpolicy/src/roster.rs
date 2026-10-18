//! The interim topic broker's subscriber table and its caps (audit H12), as a
//! structure the host can test with a counting [`Transport`].
//!
//! Every member holds a handle in the broker's table and a name in the
//! kernel's registry, so the table is bounded: [`Roster::new`] takes a total
//! and a per-caller cap (a caller without `CAP_SETUID` is limited to the
//! second, a system service only to the first). A member whose sink died is
//! dead weight for those caps and nothing will ever unsubscribe it, so
//! [`Roster::admit`] sweeps before it refuses: first the members a failed
//! delivery already released, then, when the table is still full, every
//! member's registered name is looked up and the ones that are gone are
//! dropped (a subscriber that died while nothing was published to it is
//! otherwise counted for good).

use alloc::vec::Vec;

use crate::sink::{Sink, Transport};

/// One attached subscriber: its sink, who subscribed and what the broker
/// keeps with it (the topic filter).
pub struct Member<H, X> {
    pub sink: Sink<H>,
    /// The uid that subscribed (kernel-stamped), for the per-caller cap.
    pub owner: u32,
    /// Whether it was a system service, exempt from the per-caller cap.
    pub system: bool,
    pub data: X,
}

/// The subscriber table.
pub struct Roster<H, X> {
    members: Vec<Member<H, X>>,
    max_total: usize,
    max_per_user: usize,
}

impl<H: Copy, X> Roster<H, X> {
    /// An empty table holding at most `max_total` members, `max_per_user` of
    /// them from any one unprivileged caller.
    pub fn new(max_total: usize, max_per_user: usize) -> Roster<H, X> {
        Roster {
            members: Vec::new(),
            max_total,
            max_per_user,
        }
    }

    /// Members attached.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether no member is attached.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The members, oldest first.
    pub fn members(&self) -> &[Member<H, X>] {
        &self.members
    }

    /// The members, mutable (a delivery updates a sink's handle).
    pub fn members_mut(&mut self) -> &mut [Member<H, X>] {
        &mut self.members
    }

    /// Whether a member is attached under `sink`.
    pub fn has_sink(&self, sink: &str) -> bool {
        self.members.iter().any(|member| member.sink.name() == sink)
    }

    fn has_room(&self, owner: u32, system: bool) -> bool {
        let held = self
            .members
            .iter()
            .filter(|member| !member.system && member.owner == owner)
            .count();
        self.members.len() < self.max_total && (system || held < self.max_per_user)
    }

    /// Whether a new member from `owner` fits, after dropping the members
    /// whose sink is gone. Checks liveness only when the table is full, so a
    /// healthy broker pays nothing for it.
    pub fn admit<T: Transport<Handle = H>>(
        &mut self,
        transport: &mut T,
        owner: u32,
        system: bool,
    ) -> bool {
        self.prune();
        if self.has_room(owner, system) {
            return true;
        }
        for member in &mut self.members {
            member.sink.check_alive(transport);
        }
        self.prune();
        self.has_room(owner, system)
    }

    /// Attach `member` (after [`admit`](Self::admit) said it fits).
    pub fn push(&mut self, member: Member<H, X>) {
        self.members.push(member);
    }

    /// Forget the members whose sink holds no handle any more (a delivery or
    /// a liveness check released it); nothing is left to give back.
    pub fn prune(&mut self) {
        self.members.retain(|member| member.sink.is_resolved());
    }

    /// Drop the member attached under `sink`, giving its handle back.
    pub fn detach<T: Transport<Handle = H>>(&mut self, transport: &mut T, sink: &str) {
        let mut index = 0;
        while index < self.members.len() {
            if self.members[index].sink.name() == sink {
                let mut gone = self.members.remove(index);
                gone.sink.release(transport);
            } else {
                index += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::SendFailure;
    use alloc::format;
    use alloc::string::String;

    /// A transport that counts open handles and knows which names are alive.
    struct Fake {
        open: usize,
        alive: Vec<String>,
    }

    impl Transport for Fake {
        type Handle = u32;
        fn resolve(&mut self, name: &str) -> Option<u32> {
            self.alive.iter().any(|alive| alive == name).then(|| {
                self.open += 1;
                0
            })
        }
        fn send(&mut self, _handle: u32) -> Result<(), SendFailure> {
            Ok(())
        }
        fn release(&mut self, _handle: u32) {
            assert!(self.open > 0, "a handle was released twice");
            self.open -= 1;
        }
        fn alive(&mut self, name: &str) -> bool {
            self.alive.iter().any(|alive| alive == name)
        }
    }

    fn subscribe(
        roster: &mut Roster<u32, ()>,
        fake: &mut Fake,
        name: &str,
        owner: u32,
        system: bool,
    ) -> bool {
        if !roster.admit(fake, owner, system) {
            return false;
        }
        fake.open += 1;
        fake.alive.push(name.into());
        roster.push(Member {
            sink: Sink::new(name.into(), 0),
            owner,
            system,
            data: (),
        });
        true
    }

    fn fake() -> Fake {
        Fake {
            open: 0,
            alive: Vec::new(),
        }
    }

    #[test]
    fn the_caps_hold() {
        let mut fake = fake();
        let mut roster = Roster::new(6, 2);
        assert!(subscribe(&mut roster, &mut fake, "a.1", 1000, false));
        assert!(subscribe(&mut roster, &mut fake, "a.2", 1000, false));
        assert!(
            !subscribe(&mut roster, &mut fake, "a.3", 1000, false),
            "per-caller cap"
        );
        // Another user and the system are not held by it.
        assert!(subscribe(&mut roster, &mut fake, "b.1", 1001, false));
        for n in 0..3 {
            assert!(subscribe(
                &mut roster,
                &mut fake,
                &format!("s.{n}"),
                0,
                true
            ));
        }
        assert_eq!(roster.len(), 6);
        assert!(
            !subscribe(&mut roster, &mut fake, "s.9", 0, true),
            "total cap, system included"
        );
        assert_eq!(fake.open, 6, "a refusal opened nothing");
    }

    #[test]
    fn dead_subscribers_are_swept_before_a_refusal() {
        // Fifty services restart and each leaves a member behind that nothing
        // was ever published to (so no failed delivery pruned it): the table
        // would be full of the dead and refuse a live one.
        let mut fake = fake();
        let mut roster = Roster::new(8, 8);
        for n in 0..8 {
            assert!(subscribe(
                &mut roster,
                &mut fake,
                &format!("old.{n}"),
                0,
                true
            ));
        }
        fake.alive.clear();
        assert!(subscribe(&mut roster, &mut fake, "new.1", 0, true));
        assert_eq!(roster.len(), 1, "the dead were dropped");
        assert_eq!(fake.open, 1, "and their handles given back");
        for round in 0..50 {
            fake.alive.retain(|name| name == "new.1");
            for n in 0..7 {
                assert!(subscribe(
                    &mut roster,
                    &mut fake,
                    &format!("r{round}.{n}"),
                    0,
                    true
                ));
            }
            fake.alive.clear();
        }
        assert!(roster.len() <= 8);
        assert_eq!(fake.open, roster.len());
    }

    #[test]
    fn live_members_are_never_dropped_to_make_room() {
        let mut fake = fake();
        let mut roster = Roster::new(3, 3);
        for n in 0..3 {
            assert!(subscribe(
                &mut roster,
                &mut fake,
                &format!("s.{n}"),
                0,
                true
            ));
        }
        assert!(!subscribe(&mut roster, &mut fake, "s.9", 0, true));
        assert_eq!(roster.len(), 3);
        assert_eq!(fake.open, 3);
    }

    #[test]
    fn one_users_dead_subscribers_do_not_count_against_their_cap() {
        let mut fake = fake();
        let mut roster = Roster::new(64, 2);
        assert!(subscribe(&mut roster, &mut fake, "u.1", 1000, false));
        assert!(subscribe(&mut roster, &mut fake, "u.2", 1000, false));
        fake.alive.clear();
        assert!(subscribe(&mut roster, &mut fake, "u.3", 1000, false));
        assert_eq!(roster.len(), 1);
    }

    #[test]
    fn detaching_gives_the_handle_back_once() {
        let mut fake = fake();
        let mut roster = Roster::new(4, 4);
        assert!(subscribe(&mut roster, &mut fake, "u.1", 1000, false));
        roster.detach(&mut fake, "u.1");
        roster.detach(&mut fake, "u.1");
        assert_eq!(roster.len(), 0);
        assert_eq!(fake.open, 0);
    }
}

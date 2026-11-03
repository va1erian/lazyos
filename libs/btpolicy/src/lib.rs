//! Who may use the Bluetooth radio (`docs/bluetooth-plan.md`, decisions 3,
//! 6 and 10), as data.
//!
//! `btd` runs as `_bt` with only `CAP_INPUT_SOURCE`. `usbd` serves the
//! controller's packets as `os.lazy.usb.bt` and answers `_bt` (and root) only:
//! the radio carries the keyboard and mouse of the machine, so authority over
//! it is authority over input. `btd` serves `os.lazy.bt.v1` (pairing and the
//! device list) to the person at the machine: [`may_use_api`].

#![no_std]

/// The `_bt` system user `btd` runs as: `CAP_INPUT_SOURCE`, nothing else.
/// (901 `_snd` .. 915 `_gpu` are taken; the next free uid is 917.)
pub const BT_UID: u32 = 916;

/// `elevd`'s system uid (`accountdb::ELEVD_UID`, checked in the tests).
pub const ELEVD_UID: u32 = 909;
/// The range of account (user) uids (`accountdb::FIRST_UID..=LAST_UID`).
pub const FIRST_USER_UID: u32 = 1000;
pub const LAST_USER_UID: u32 = 59_999;

/// Whether `uid` may call `os.lazy.usb.bt.v1`: `_bt` and root (the kernel
/// stamps the uid, a request body cannot name a caller).
pub const fn may_use_transport(uid: u32) -> bool {
    uid == 0 || uid == BT_UID
}

/// What a caller of `os.lazy.bt.v1` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `List`: which devices are known and connected.
    List,
    /// `Status` and `Subscribe`: they carry the passkey or number of a
    /// pairing in progress, which is for the person at the machine.
    Observe,
    /// `StartDiscovery`, `StopDiscovery`, `Pair`, `ConfirmNumber`,
    /// `CancelPairing` and `Forget`: they change which devices may type into
    /// every session.
    Control,
}

/// The kernel-stamped identity of a caller (a request body cannot name it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub label_id: u32,
    pub session: u64,
}

impl Caller {
    /// A user of the local console session: an account uid inside a login
    /// session. `logind` opens exactly one session at a time, on the machine's
    /// own console, and `init` ends the tasks of a session when it ends, so a
    /// session id is the proof of "sitting at this machine". Root is not one
    /// (nobody is root), nor is a system service.
    pub const fn is_session_user(self) -> bool {
        self.session != 0 && self.uid >= FIRST_USER_UID && self.uid <= LAST_USER_UID
    }

    /// `elevd` (its uid, unlabelled, outside any session): it performs what an
    /// administrator approved on the trusted prompt
    /// (`elevpolicy::is_elevd`).
    pub const fn is_elevd(self) -> bool {
        self.uid == ELEVD_UID && self.label_id == 0 && self.session == 0
    }
}

/// Whether `caller` may do `op` on `os.lazy.bt.v1`. `List` is open to any
/// session user; everything that shows a passkey or changes which devices may
/// type is for the session user or `elevd` (an administrator-approved
/// operation) too. Everyone else gets `EACCES`: `_bt` itself, other services,
/// root and callers outside a session. Developer tools are kept out one
/// layer earlier: the interface is in `devtool::DENY`.
pub const fn may_use_api(caller: Caller, op: Op) -> bool {
    match op {
        Op::List | Op::Observe | Op::Control => caller.is_session_user() || caller.is_elevd(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_USER: Caller = Caller {
        uid: 1000,
        label_id: 0,
        session: 3,
    };
    const OPS: [Op; 3] = [Op::List, Op::Observe, Op::Control];

    #[test]
    fn only_bt_and_root_may_use_the_transport() {
        assert!(may_use_transport(BT_UID));
        assert!(may_use_transport(0));
        for uid in [1, 904, 915, 917, 1000, u32::MAX] {
            assert!(!may_use_transport(uid), "{uid}");
        }
    }

    #[test]
    fn the_uid_is_a_system_uid() {
        const { assert!(BT_UID > 915 && BT_UID < 1000) };
    }

    #[test]
    fn a_session_user_may_do_everything() {
        for op in OPS {
            assert!(may_use_api(SESSION_USER, op), "{op:?}");
        }
    }

    #[test]
    fn elevd_may_do_everything_only_as_itself() {
        let elevd = Caller {
            uid: ELEVD_UID,
            label_id: 0,
            session: 0,
        };
        for op in OPS {
            assert!(may_use_api(elevd, op));
            // Labelled, or inside a session, it is not the elevator.
            let labelled = Caller {
                label_id: 4,
                ..elevd
            };
            let in_session = Caller {
                session: 1,
                ..elevd
            };
            assert!(!may_use_api(labelled, op));
            assert!(!may_use_api(in_session, op));
        }
    }

    #[test]
    fn nobody_else_may_call() {
        let c = |uid, session| Caller {
            uid,
            label_id: 0,
            session,
        };
        let nobody = [
            // Root, even inside a session.
            c(0, 3),
            // The radio's own service and other services.
            c(BT_UID, 0),
            c(904, 0),
            // A user with no login session (a service running as them, or
            // anything before login).
            c(1000, 0),
            // Out of the account range.
            c(999, 3),
            c(LAST_USER_UID + 1, 3),
            c(u32::MAX, u64::MAX),
        ];
        for caller in nobody {
            for op in OPS {
                assert!(!may_use_api(caller, op), "{caller:?} {op:?}");
            }
        }
    }

    #[test]
    fn the_uid_constants_match_the_account_database() {
        assert_eq!(ELEVD_UID, accountdb::ELEVD_UID);
        assert_eq!(FIRST_USER_UID, accountdb::FIRST_UID);
        assert_eq!(LAST_USER_UID, accountdb::LAST_UID);
    }
}

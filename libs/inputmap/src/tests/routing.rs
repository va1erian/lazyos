//! Surface, session and focus routing.

use crate::router::{Error, FocusChange, Router, MAX_SESSIONS_PER_OWNER};

fn router() -> Router {
    let mut router = Router::new();
    router.register_surface(1, 100).unwrap();
    router.register_surface(2, 200).unwrap();
    router
}

#[test]
fn a_task_can_only_open_its_own_surface() {
    let mut router = router();
    assert_eq!(router.open(200, 1), Err(Error::NotOwner));
    assert_eq!(router.open(100, 9), Err(Error::NoSurface));
    let opened = router.open(100, 1).unwrap();
    assert!(opened.first_for_surface && !opened.focused);
    assert_eq!(router.session(opened.session).unwrap().surface, 1);
}

#[test]
fn only_the_focused_session_receives_keys() {
    let mut router = router();
    let a = router.open(100, 1).unwrap().session;
    let b = router.open(200, 2).unwrap().session;
    assert_eq!(router.focused_session(), None);
    assert_eq!(
        router.set_focus(Some(1)),
        FocusChange {
            left: None,
            entered: Some(a)
        }
    );
    assert_eq!(router.focused_session(), Some(a));
    assert_eq!(
        router.set_focus(Some(2)),
        FocusChange {
            left: Some(a),
            entered: Some(b)
        }
    );
    assert_eq!(router.focused_session(), Some(b));
    // No change is not a change: nobody re-enters.
    assert_eq!(router.set_focus(Some(2)), FocusChange::default());
    // Focus with nobody focused: the old holder leaves, nobody gets keys.
    assert_eq!(
        router.set_focus(None),
        FocusChange {
            left: Some(b),
            entered: None
        }
    );
    assert_eq!(router.focused_session(), None);
}

#[test]
fn focusing_a_legacy_surface_still_takes_the_keyboard_from_a_session() {
    let mut router = router();
    let a = router.open(100, 1).unwrap().session;
    router.set_focus(Some(1));
    // Surface 2 has no session (a legacy client): `a` leaves, no one enters.
    let change = router.set_focus(Some(2));
    assert_eq!(
        change,
        FocusChange {
            left: Some(a),
            entered: None
        }
    );
    assert_eq!(router.focused_session(), None);
}

#[test]
fn a_session_opened_on_the_focused_surface_is_entered_at_once() {
    let mut router = router();
    router.set_focus(Some(1));
    let opened = router.open(100, 1).unwrap();
    assert!(opened.focused);
    assert_eq!(router.focused_session(), Some(opened.session));
}

#[test]
fn reopening_replaces_the_old_session() {
    let mut router = router();
    let first = router.open(100, 1).unwrap();
    let second = router.open(100, 1).unwrap();
    assert_eq!(second.replaced, Some(first.session));
    assert!(!second.first_for_surface);
    assert!(router.session(first.session).is_none());
    assert_eq!(router.sessions().count(), 1);
}

#[test]
fn closing_needs_the_owner_and_frees_the_surface() {
    let mut router = router();
    let opened = router.open(100, 1).unwrap();
    assert_eq!(router.close(opened.session, 200), Err(Error::NoSession));
    assert_eq!(router.close(opened.session, 100), Ok(1));
    assert!(!router.has_session(1));
    assert_eq!(router.close(opened.session, 100), Err(Error::NoSession));
    // The surface stays registered: it can be opened again.
    assert!(router.open(100, 1).is_ok());
}

#[test]
fn destroying_a_surface_closes_its_session_and_clears_focus() {
    let mut router = router();
    let opened = router.open(100, 1).unwrap();
    router.set_focus(Some(1));
    assert_eq!(router.unregister_surface(1), Some(opened.session));
    assert_eq!(router.focused_session(), None);
    assert_eq!(router.open(100, 1), Err(Error::NoSurface));
    assert_eq!(router.unregister_surface(1), None);
}

#[test]
fn tables_are_bounded() {
    let mut router = Router::new();
    for surface in 0..MAX_SESSIONS_PER_OWNER as u64 + 1 {
        router.register_surface(surface, 7).unwrap();
    }
    for surface in 0..MAX_SESSIONS_PER_OWNER as u64 {
        router.open(7, surface).unwrap();
    }
    assert_eq!(
        router.open(7, MAX_SESSIONS_PER_OWNER as u64),
        Err(Error::Full)
    );
    // Re-opening a held surface replaces, so it does not count against the cap.
    assert!(router.open(7, 0).is_ok());
    // The surface table is bounded too.
    let mut router = Router::new();
    let mut refused = 0;
    for surface in 0..1_000u64 {
        refused += router.register_surface(surface, 1).is_err() as u32;
    }
    assert!(refused > 0);
}

#[test]
fn dead_endpoint_removal_is_owner_agnostic() {
    let mut router = router();
    let opened = router.open(100, 1).unwrap();
    let removed = router.remove(opened.session).unwrap();
    assert_eq!((removed.owner, removed.surface), (100, 1));
    assert!(router.remove(opened.session).is_none());
    assert!(!router.has_session(1));
}

/// Churn: sessions opened, focused, replaced and torn down in odd orders
/// never leave a dangling index.
#[test]
fn churn_keeps_the_indexes_consistent() {
    let mut router = Router::new();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..50_000 {
        let surface = next() % 12;
        let owner = surface % 3;
        match next() % 6 {
            0 => {
                let _ = router.register_surface(surface, owner);
            }
            1 => {
                let _ = router.open(owner, surface);
            }
            2 => {
                router.set_focus(Some(surface));
            }
            3 => {
                router.set_focus(None);
            }
            4 => {
                router.unregister_surface(surface);
            }
            _ => {
                let picked = router.sessions().nth((next() % 4) as usize);
                if let Some(session) = picked {
                    let owner = router.session(session).unwrap().owner;
                    let _ = router.close(session, owner);
                }
            }
        }
        for session in router.sessions().collect::<alloc::vec::Vec<_>>() {
            let found = router.session(session).unwrap();
            assert!(router.has_session(found.surface));
        }
        if let Some(focused) = router.focused_session() {
            assert!(router.session(focused).is_some());
        }
    }
}

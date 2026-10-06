//! The console session (issue #396): a sessionless session that takes the
//! keyboard only while no compositor is attached.

use crate::router::{Error, FocusChange, Router};

#[test]
fn without_a_compositor_the_console_gets_the_keys() {
    let mut router = Router::new();
    let opened = router.open_console(7).unwrap();
    assert!(
        opened.focused,
        "nothing can be focused without a compositor"
    );
    assert!(!opened.first_for_surface);
    assert_eq!(router.focused_session(), Some(opened.session));
    assert_eq!(router.session(opened.session).unwrap().surface, None);
    assert_eq!(router.console(), Some(opened.session));
}

#[test]
fn a_compositor_takes_the_keyboard_and_gives_it_back() {
    let mut router = Router::new();
    let console = router.open_console(7).unwrap().session;
    assert_eq!(
        router.set_compositor(true),
        FocusChange {
            left: Some(console),
            entered: None
        }
    );
    assert_eq!(router.focused_session(), None);
    // Under a compositor, no focus means nobody: never the console.
    router.register_surface(1, 100).unwrap();
    let window = router.open(100, 1).unwrap().session;
    router.set_focus(Some(1));
    assert_eq!(router.focused_session(), Some(window));
    assert_eq!(router.set_focus(None).entered, None);
    // The compositor dies: focus is gone and the console has the keys again.
    router.set_focus(Some(1));
    assert_eq!(
        router.set_compositor(false),
        FocusChange {
            left: Some(window),
            entered: Some(console)
        }
    );
    assert_eq!(router.focused_session(), Some(console));
}

#[test]
fn opened_under_a_compositor_it_waits() {
    let mut router = Router::new();
    router.set_compositor(true);
    let opened = router.open_console(7).unwrap();
    assert!(!opened.focused);
    assert_eq!(router.focused_session(), None);
}

#[test]
fn one_console_holder_at_a_time() {
    let mut router = Router::new();
    let first = router.open_console(7).unwrap().session;
    assert_eq!(router.open_console(8), Err(Error::Busy));
    // The holder reopening replaces its own session.
    let again = router.open_console(7).unwrap();
    assert_eq!(again.replaced, Some(first));
    assert!(router.session(first).is_none());
    // Closed, it is free for anyone.
    assert_eq!(router.close(again.session, 7), Ok(None));
    assert_eq!(router.console(), None);
    assert_eq!(router.focused_session(), None);
    assert!(router.open_console(8).is_ok());
}

#[test]
fn only_its_owner_closes_it_and_a_dead_endpoint_frees_it() {
    let mut router = Router::new();
    let console = router.open_console(7).unwrap().session;
    assert_eq!(router.close(console, 8), Err(Error::NoSession));
    let removed = router.remove(console).unwrap();
    assert_eq!((removed.owner, removed.surface), (7, None));
    assert_eq!(router.console(), None);
}

#[test]
fn the_console_does_not_count_against_window_sessions() {
    let mut router = Router::new();
    router.open_console(7).unwrap();
    for surface in 0..crate::router::MAX_SESSIONS_PER_OWNER as u64 {
        router.register_surface(surface, 7).unwrap();
        router.open(7, surface).unwrap();
    }
}

/// Random opens, closes, focus moves and compositor comings and goings: the
/// keyboard always goes to exactly one live session or none, the console only
/// without a compositor, and a window only while it is focused.
#[test]
fn soak_console_and_windows() {
    let mut router = Router::new();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut compositor = false;
    for _ in 0..100_000 {
        let surface = next() % 8;
        match next() % 8 {
            0 => {
                let _ = router.register_surface(surface, surface % 3);
            }
            1 => {
                let _ = router.open(surface % 3, surface);
            }
            2 => {
                let _ = router.open_console(next() % 2);
            }
            3 => {
                router.set_focus(Some(surface));
            }
            4 => {
                router.set_focus(None);
            }
            5 => {
                compositor = next() % 2 == 0;
                router.set_compositor(compositor);
            }
            6 => {
                router.unregister_surface(surface);
            }
            _ => {
                let picked = router.sessions().nth((next() % 4) as usize);
                if let Some(session) = picked {
                    router.remove(session);
                }
            }
        }
        if let Some(focused) = router.focused_session() {
            let session = router
                .session(focused)
                .expect("the focused session is live");
            if session.surface.is_none() {
                assert!(
                    !compositor,
                    "the console never takes keys under a compositor"
                );
                assert_eq!(router.console(), Some(focused));
            }
        }
        if let Some(console) = router.console() {
            assert_eq!(router.session(console).unwrap().surface, None);
        }
    }
}

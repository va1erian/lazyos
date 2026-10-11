//! Identity, the rate limit and the user's policy.

use super::*;

const SHELL: Shell = Shell {
    uid: Some(1000),
    session: Some(3),
};

fn sender(uid: u32, session: u64, label: u32, pid: u64) -> Sender {
    Sender {
        caller: Caller { uid, session },
        label,
        pid,
    }
}

#[test]
fn the_sender_is_the_app_init_launched_or_a_script_in_the_session() {
    let rows = [("os.lazy.files", 41u64), ("logd", 9)];
    let is_app = |name: &str| name.starts_with("os.lazy.");
    assert_eq!(
        identify(sender(1000, 3, 7, 41), SHELL, rows, is_app),
        Ok(Owner::App("os.lazy.files"))
    );
    // An unlabelled program in the session (the Terminal's child).
    assert_eq!(
        identify(sender(1000, 3, 0, 500), SHELL, rows, is_app),
        Ok(Owner::Script)
    );
    // A labelled program that init does not list is nobody: it cannot post
    // as the Terminal either.
    assert_eq!(
        identify(sender(1000, 3, 7, 500), SHELL, rows, is_app),
        Err(Deny::NotAnApp)
    );
    // A supervised service is no app, and with label 0 it is in session 0.
    assert_eq!(
        identify(sender(0, 0, 0, 9), SHELL, rows, is_app),
        Err(Deny::Session)
    );
    assert_eq!(
        identify(sender(1001, 3, 0, 41), SHELL, rows, is_app),
        Err(Deny::Uid)
    );
    // No login: session 0 has no script fallback, so a service cannot post
    // as the Terminal there.
    let no_login = Shell {
        uid: Some(0),
        session: Some(0),
    };
    assert_eq!(
        identify(sender(0, 0, 0, 9), no_login, rows, is_app),
        Err(Deny::NotAnApp)
    );
    assert_eq!(
        identify(
            sender(1000, 3, 0, 41),
            Shell { uid: None, ..SHELL },
            rows,
            is_app
        ),
        Err(Deny::Unknown)
    );
}

#[test]
fn an_owner_key_is_never_an_app_id() {
    assert_eq!(Owner::Script.key(), SCRIPT_OWNER);
    assert_eq!(Owner::App("os.lazy.files").key(), "os.lazy.files");
    // Neither reserved key can be a package's `system_name`
    // (reverse-DNS words with dots).
    assert!(!SCRIPT_OWNER.contains('.') && !SYSTEM_OWNER.contains('.'));
}

#[test]
fn the_bucket_allows_a_burst_then_one_per_interval() {
    let mut limit = RateLimit::default();
    for _ in 0..RATE_BURST {
        assert!(limit.take("a", 0));
    }
    assert!(!limit.take("a", 0));
    assert!(!limit.take("a", RATE_REFILL_TICKS - 1));
    assert!(limit.take("a", RATE_REFILL_TICKS));
    assert!(!limit.take("a", RATE_REFILL_TICKS));
    // Another sender has its own bucket.
    assert!(limit.take("b", 0));
    // A long quiet spell refills to the burst, not beyond it.
    let later = 100 * RATE_REFILL_TICKS;
    for _ in 0..RATE_BURST {
        assert!(limit.take("a", later));
    }
    assert!(!limit.take("a", later));
}

#[test]
fn buckets_are_bounded() {
    let mut limit = RateLimit::default();
    for n in 0..(BUCKETS_MAX * 3) {
        assert!(limit.take(&format!("app{n}"), n as u64));
        assert!(limit.len() <= BUCKETS_MAX);
    }
    assert!(!limit.is_empty());
}

#[test]
fn the_policy_mutes_apps_and_holds_back_all_but_errors_in_dnd() {
    let policy = Policy {
        dnd: false,
        muted: Policy::parse_muted("org.noisy,  org.other\n"),
    };
    assert_eq!(policy.muted, ["org.noisy", "org.other"]);
    assert!(policy.suppresses("org.noisy", Level::Error));
    assert!(!policy.suppresses("org.quiet", Level::Info));
    let dnd = Policy {
        dnd: true,
        muted: Vec::new(),
    };
    assert!(dnd.suppresses("a", Level::Info));
    assert!(dnd.suppresses("a", Level::Warn));
    assert!(!dnd.suppresses("a", Level::Error));
    // The shell's own notices are never held back.
    let all = Policy {
        dnd: true,
        muted: vec![SYSTEM_OWNER.into()],
    };
    assert!(!all.suppresses(SYSTEM_OWNER, Level::Info));
}

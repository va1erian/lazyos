//! The queue: places, waiting, coalescing, replace in place, timeouts, the
//! sender's and the user's closes, and the muted.

use super::*;

#[test]
fn a_posted_toast_takes_a_place_and_times_out() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Info, "Hello"), 0).unwrap();
    assert_eq!(center.live().len(), 1);
    assert!(center
        .take_events()
        .iter()
        .any(|e| e.id == id && e.kind == EventKind::Shown));
    measured(&mut center, 0);
    // Life starts when it is placed: 5 s of ticks.
    center.tick(499);
    assert_eq!(center.live().len(), 1);
    center.tick(500);
    assert!(center.live().is_empty());
    assert_eq!(closed(&mut center), [(id, CloseReason::Timeout)]);
}

#[test]
fn warn_and_error_stay_until_dismissed() {
    let mut center = Center::new();
    let warn = center.post("a", req(Level::Warn, "Low disk"), 0).unwrap();
    let error = center.post("a", req(Level::Error, "Failed"), 0).unwrap();
    measured(&mut center, 0);
    center.tick(1_000_000);
    assert_eq!(center.live().len(), 2);
    center.dismiss(warn, 1_000_001);
    assert_eq!(closed(&mut center), [(warn, CloseReason::User)]);
    assert!(center.get(error).is_some());
}

#[test]
fn hovering_pauses_the_timeout_and_leaving_resumes_it() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Info, "x"), 0).unwrap();
    measured(&mut center, 0);
    center.tick(300);
    center.hover(id, true, 300);
    // 200 ticks were left; a long hover does not use them.
    center.tick(10_000);
    assert_eq!(center.live().len(), 1);
    center.hover(id, false, 10_000);
    center.tick(10_199);
    assert_eq!(center.live().len(), 1);
    center.tick(10_200);
    assert!(center.live().is_empty());
}

#[test]
fn the_countdown_fraction_shows_only_while_hovered() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Info, "x"), 0).unwrap();
    measured(&mut center, 0);
    let frame = |c: &Center, now| {
        c.frames(1280, now)
            .into_iter()
            .find(|f| f.id == id)
            .unwrap()
    };
    assert_eq!(frame(&center, 10).countdown, None);
    center.hover(id, true, 250);
    let countdown = frame(&center, 250).countdown.expect("hovered");
    assert!((countdown - 0.5).abs() < 0.01, "{countdown}");
}

#[test]
fn a_sender_cannot_fill_the_screen() {
    let mut center = Center::new();
    for n in 0..4 {
        center
            .post("a", req(Level::Warn, &format!("w{n}")), 0)
            .unwrap();
    }
    assert_eq!(center.live().len(), OWNER_VISIBLE_MAX);
    // Another app still gets the free place.
    center.post("b", req(Level::Warn, "other"), 0).unwrap();
    assert_eq!(center.live().len(), VISIBLE_MAX);
    assert!(center.live().iter().any(|t| t.owner == "b"));
    assert_eq!(center.waiting(), 1);
}

#[test]
fn the_global_cap_queues_and_a_freed_place_is_refilled_in_order() {
    let mut center = Center::new();
    let ids: Vec<u64> = (0..6)
        .map(|n| {
            let owner = format!("app{n}");
            center.post(&owner, req(Level::Error, "e"), 0).unwrap()
        })
        .collect();
    assert_eq!(center.live().len(), VISIBLE_MAX);
    assert_eq!(center.waiting(), 2);
    measured(&mut center, 0);
    center.dismiss(ids[0], 5);
    let live: Vec<u64> = center.live().iter().map(|t| t.id).collect();
    assert_eq!(live, [ids[1], ids[2], ids[3], ids[4]]);
    assert_eq!(center.waiting(), 1);
}

#[test]
fn a_burst_of_plain_notices_becomes_one_summary() {
    let mut center = Center::new();
    // Fill the screen with other apps' errors so the burst has to wait.
    for n in 0..VISIBLE_MAX {
        center
            .post(&format!("x{n}"), req(Level::Error, "e"), 0)
            .unwrap();
    }
    let burst: Vec<u64> = (0..4)
        .map(|n| {
            center
                .post("usb", req(Level::Info, &format!("Device {n} connected")), 0)
                .unwrap()
        })
        .collect();
    assert_eq!(center.waiting(), 4);
    measured(&mut center, 0);
    let first = center.live()[0].id;
    center.dismiss(first, 1);
    // One place freed: the four became one summary standing for all.
    assert_eq!(center.waiting(), 0);
    let summary = center.live().last().unwrap().clone();
    assert_eq!(summary.owner, "usb");
    assert_eq!(summary.merged, 4);
    assert_eq!(summary.note.title, "4 notifications");
    assert_eq!(summary.note.body, "Device 3 connected");
    // The others were told they were replaced; the summary's id lives on.
    let replaced: Vec<u64> = closed(&mut center)
        .into_iter()
        .filter(|(_, reason)| *reason == CloseReason::Replaced)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(replaced, [burst[2], burst[1], burst[0]]);
    assert_eq!(summary.id, burst[3]);
}

#[test]
fn notices_with_actions_or_progress_or_errors_are_not_merged() {
    let mut center = Center::new();
    for n in 0..VISIBLE_MAX {
        center
            .post(&format!("x{n}"), req(Level::Error, "e"), 0)
            .unwrap();
    }
    for n in 0..5 {
        let mut r = req(Level::Info, &format!("n{n}"));
        r.note = r.note.with_actions(&[(1, "Open")]);
        center.post("a", r, 0).unwrap();
    }
    assert_eq!(center.waiting(), 5);
    measured(&mut center, 0);
    let first = center.live()[0].id;
    center.dismiss(first, 1);
    let promoted = center.live().last().unwrap();
    assert_eq!(promoted.merged, 1);
    assert_eq!(promoted.note.title, "n0");
}

#[test]
fn replace_in_place_keeps_the_id_and_the_place() {
    let mut center = Center::new();
    let mut copy = req(Level::Info, "Copying...");
    copy.note.progress = Some(10);
    let id = center.post("files", copy, 0).unwrap();
    let other = center.post("files", req(Level::Info, "Other"), 0).unwrap();
    measured(&mut center, 0);
    let before = center.get(id).unwrap().rev;
    let mut done = req(Level::Success, "Done");
    done.note.progress = Some(100);
    done.replaces = Some(id);
    assert_eq!(center.post("files", done, 40), Ok(id));
    let toast = center.get(id).unwrap();
    assert_eq!(toast.note.title, "Done");
    assert_eq!(toast.rev, before + 1);
    // Still the first (oldest) one on screen, and no new toast was made.
    assert_eq!(center.live().len(), 2);
    assert_eq!(center.live()[0].id, id);
    assert_eq!(center.live()[1].id, other);
    // Finished progress lets it time out, 5 s from the update.
    center.tick(40 + 499);
    assert!(center.get(id).is_some());
    center.tick(40 + 500);
    assert!(center.get(id).is_none());
}

#[test]
fn replacing_someone_elses_or_a_gone_toast_makes_a_new_one() {
    let mut center = Center::new();
    let mine = center.post("a", req(Level::Warn, "mine"), 0).unwrap();
    let mut steal = req(Level::Warn, "hijack");
    steal.replaces = Some(mine);
    let new = center.post("b", steal, 0).unwrap();
    assert_ne!(new, mine);
    assert_eq!(center.get(mine).unwrap().note.title, "mine");
    let mut stale = req(Level::Warn, "again");
    stale.replaces = Some(9999);
    let fresh = center.post("a", stale, 0).unwrap();
    assert_ne!(fresh, 9999);
}

#[test]
fn the_sender_closes_only_its_own() {
    let mut center = Center::new();
    let id = center.post("a", req(Level::Warn, "x"), 0).unwrap();
    assert!(!center.close("b", id, 1));
    assert!(center.get(id).is_some());
    assert!(center.close("a", id, 1));
    assert_eq!(closed(&mut center), [(id, CloseReason::Sender)]);
    // Closing again is harmless.
    assert!(!center.close("a", id, 2));
    assert!(closed(&mut center).is_empty());
}

#[test]
fn clicks_tell_the_sender_and_close_the_toast() {
    let mut center = Center::new();
    let mut r = req(Level::Error, "Failed");
    r.note = r.note.with_actions(&[(1, "Retry"), (2, "Details")]);
    let id = center.post("a", r, 0).unwrap();
    center.take_events();
    // An action the toast does not have does nothing.
    center.invoke(id, 9, 1);
    assert!(center.take_events().is_empty());
    center.invoke(id, 2, 1);
    let kinds: Vec<EventKind> = center.take_events().into_iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [EventKind::Action(2), EventKind::Closed(CloseReason::User)]
    );
    let id = center.post("a", req(Level::Error, "Again"), 2).unwrap();
    center.take_events();
    center.activate(id, 3);
    let kinds: Vec<EventKind> = center.take_events().into_iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [EventKind::Activated, EventKind::Closed(CloseReason::User)]
    );
}

#[test]
fn rate_limit_and_alive_cap_refuse_a_flood() {
    let mut center = Center::new();
    let mut accepted = 0;
    let mut limited = 0;
    for n in 0..20 {
        match center.post("a", req(Level::Info, &format!("n{n}")), 0) {
            Ok(_) => accepted += 1,
            Err(Refused::RateLimited) => limited += 1,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert_eq!(accepted, RATE_BURST);
    assert_eq!(limited, 20 - RATE_BURST);
    // Over time the alive cap is what stops it.
    let mut now = 0;
    let mut refused = None;
    for n in 0..(OWNER_ALIVE_MAX * 2) {
        now += RATE_REFILL_TICKS;
        if let Err(why) = center.post("sticky", req(Level::Error, &format!("e{n}")), now) {
            refused = Some(why);
            break;
        }
    }
    assert_eq!(refused, Some(Refused::Full));
    // Other senders are unaffected.
    assert!(center.post("b", req(Level::Info, "fine"), now).is_ok());
}

#[test]
fn waiting_is_bounded_and_evicts_plain_notices_first() {
    let mut center = Center::new();
    for n in 0..VISIBLE_MAX {
        center
            .post(&format!("x{n}"), req(Level::Error, "e"), 0)
            .unwrap();
    }
    let mut now = 0;
    for n in 0..(WAITING_MAX + 8) {
        now += RATE_REFILL_TICKS;
        let owner = format!("w{}", n % 20);
        let level = if n % 5 == 0 {
            Level::Error
        } else {
            Level::Info
        };
        let _ = center.post(&owner, req(level, &format!("n{n}")), now);
        assert!(center.waiting() <= WAITING_MAX);
    }
    assert_eq!(center.waiting(), WAITING_MAX);
}

#[test]
fn muted_and_dnd_toasts_are_accepted_but_never_shown() {
    let mut center = Center::new();
    center.set_policy(Policy {
        dnd: true,
        muted: vec!["loud".into()],
    });
    let info = center.post("a", req(Level::Info, "x"), 0).unwrap();
    let loud = center.post("loud", req(Level::Error, "x"), 0).unwrap();
    let error = center.post("a", req(Level::Error, "real"), 0).unwrap();
    assert_eq!(center.live().len(), 1);
    assert_eq!(center.live()[0].id, error);
    let muted: Vec<u64> = closed(&mut center)
        .into_iter()
        .filter(|(_, reason)| *reason == CloseReason::Muted)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(muted, [info, loud]);
    // The shell's own notice shows through everything.
    let id = center.post_system(req(Level::Info, "App stopped"), 0);
    assert!(center.live().iter().any(|t| t.id == id));
}

#[test]
fn a_restart_closes_everything_with_the_restart_reason() {
    let mut center = Center::new();
    let a = center.post("a", req(Level::Error, "x"), 0).unwrap();
    for n in 0..VISIBLE_MAX {
        center
            .post(&format!("x{n}"), req(Level::Error, "y"), 0)
            .unwrap();
    }
    center.take_events();
    center.restart(10);
    assert!(center.live().is_empty() && center.waiting() == 0);
    let reasons = closed(&mut center);
    assert_eq!(reasons.len(), VISIBLE_MAX + 1);
    assert!(reasons.iter().all(|(_, r)| *r == CloseReason::Restart));
    assert!(reasons.iter().any(|(id, _)| *id == a));
}

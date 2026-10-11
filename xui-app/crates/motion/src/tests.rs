use super::*;

const CURVES: [Curve; 4] = [
    Curve::Linear,
    Curve::EaseOut,
    Curve::EaseInOut,
    Curve::Spring(Spring::TOAST),
];

#[test]
fn every_curve_runs_from_zero_to_one() {
    for curve in CURVES {
        assert!(curve.at(0.0).abs() < 1e-4, "{curve:?} start");
        assert!((curve.at(1.0) - 1.0).abs() < 0.02, "{curve:?} end");
        // Out of range progress is clamped, never extrapolated.
        assert_eq!(curve.at(-3.0), curve.at(0.0));
        assert_eq!(curve.at(9.0), curve.at(1.0));
        assert_eq!(curve.at(f32::NAN), curve.at(0.0));
    }
}

#[test]
fn eased_curves_never_decrease() {
    for curve in [Curve::Linear, Curve::EaseOut, Curve::EaseInOut] {
        let mut last = 0.0;
        for step in 0..=100 {
            let x = curve.at(step as f32 / 100.0);
            assert!(x >= last - 1e-6, "{curve:?} at {step}");
            last = x;
        }
    }
}

#[test]
fn ease_in_out_is_symmetric_about_the_middle() {
    for step in 0..=10 {
        let t = step as f32 / 10.0;
        let sum = ease_in_out_cubic(t) + ease_in_out_cubic(1.0 - t);
        assert!((sum - 1.0).abs() < 1e-5);
    }
    assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
}

#[test]
fn the_toast_spring_overshoots_slightly_then_settles() {
    let spring = Spring::TOAST;
    let peak = spring.peak();
    // "Slight overshoot": a few percent, not a bounce.
    assert!(peak > 1.02 && peak < 1.10, "peak {peak}");
    let settle = spring.settle_ms();
    assert!((200..=500).contains(&settle), "settles in {settle} ms");
    // The sampled maximum is the closed-form peak.
    let sampled = (0..settle)
        .map(|ms| spring.at(ms as f32))
        .fold(0.0f32, f32::max);
    assert!((sampled - peak).abs() < 0.01, "{sampled} vs {peak}");
    // It starts at rest: the first millisecond barely moves.
    assert!(spring.at(1.0) < 0.001);
    // After the settle time it is within 1% of the target and stays there.
    for ms in settle..settle + 500 {
        assert!((spring.at(ms as f32) - 1.0).abs() <= 0.011);
    }
}

#[test]
fn a_critically_damped_spring_does_not_overshoot() {
    let spring = Spring {
        omega: 0.02,
        zeta: 0.999,
    };
    for ms in 0..2000 {
        assert!(spring.at(ms as f32) <= 1.0 + 1e-3);
    }
}

#[test]
fn a_tween_holds_its_ends_and_moves_between() {
    let tween = Tween::new(100, 20, 1000, 200, Curve::Linear);
    assert_eq!(tween.value(0), 100);
    assert_eq!(tween.value(1000), 100);
    assert_eq!(tween.value(1100), 60);
    assert_eq!(tween.value(1200), 20);
    assert_eq!(tween.value(99_999), 20);
    assert!(!tween.done(1199));
    assert!(tween.done(1200));
    assert_eq!(tween.remaining(1050), 150);
    assert_eq!(tween.remaining(5000), 0);
}

#[test]
fn a_spring_between_extreme_values_stays_in_range() {
    let spring = Curve::Spring(Spring::TOAST);
    let tween = Tween::new(i32::MIN, i32::MAX, 0, 300, spring);
    let mut last = tween.value(0);
    for ms in 1..=300 {
        let value = tween.value(ms);
        // Overshoot clamps at the limit; a wrap would jump to the far end.
        assert!(
            i64::from(value) >= i64::from(last) - (1 << 30),
            "{ms}: {last} -> {value}"
        );
        last = value;
    }
    assert_eq!(tween.value(300), i32::MAX);
}

#[test]
fn a_zero_duration_tween_is_already_there() {
    let tween = Tween::new(5, 9, 77, 0, Curve::EaseOut);
    assert!(tween.done(0));
    assert_eq!(tween.value(0), 9);
    assert_eq!(Tween::at_rest(4).value(123), 4);
}

#[test]
fn retargeting_mid_flight_does_not_jump() {
    let mut tween = Tween::new(0, 100, 0, 100, Curve::Linear);
    let before = tween.value(40);
    tween.retarget(40, 0, 100, Curve::Linear);
    assert_eq!(tween.value(40), before);
    assert_eq!(tween.target(), 0);
    assert_eq!(tween.value(140), 0);
    // Aiming at the same target is a no-op (the clock is not restarted).
    let same = tween;
    tween.retarget(90, 0, 100, Curve::Linear);
    assert_eq!(tween, same);
}

#[test]
fn the_motion_setting_parses_and_unknown_means_full() {
    assert_eq!(Motion::parse("full"), Motion::Full);
    assert_eq!(Motion::parse("reduced"), Motion::Reduced);
    assert_eq!(Motion::parse(" none\n"), Motion::None);
    assert_eq!(Motion::parse(""), Motion::Full);
    assert_eq!(Motion::parse("fast"), Motion::Full);
    for motion in [Motion::Full, Motion::Reduced, Motion::None] {
        assert_eq!(Motion::parse(motion.as_str()), motion);
    }
    assert!(Motion::Full.travels());
    assert!(!Motion::Reduced.travels() && !Motion::None.travels());
}

#[test]
fn the_setting_shortens_or_removes_the_tween() {
    let spring = Curve::Spring(Spring::TOAST);
    let full = Motion::Full.tween(0, 50, 0, 300, spring);
    assert!(!full.done(299));
    let reduced = Motion::Reduced.tween(0, 50, 0, 300, spring);
    assert!(reduced.done(Motion::REDUCED_MS as u64));
    // A reduced tween never overshoots: it is linear.
    for ms in 0..=Motion::REDUCED_MS as u64 {
        assert!((0..=50).contains(&reduced.value(ms)));
    }
    let none = Motion::None.tween(0, 50, 0, 300, spring);
    assert!(none.done(0));
    assert_eq!(none.value(0), 50);
}

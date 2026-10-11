//! The ball in flight: carries, determinism, wind, spin, uneven ground.

use xui_golf::math::{Vec2, Vec3};
use xui_golf::play::{simulate, Event, Flight, Wind, CLUBS};

fn flat(_: f32, _: f32) -> f32 {
    0.0
}

const EAST: Vec2 = Vec2::new(1.0, 0.0);

fn shot(club: usize, power: f32, wind: Wind, side: f32) -> Flight {
    let launch = CLUBS[club].launch(Vec3::default(), EAST, power, side);
    simulate(&launch, wind, flat)
}

#[test]
fn every_club_carries_its_table_distance() {
    for (i, club) in CLUBS.iter().enumerate() {
        let carry = shot(i, 1.0, Wind::default(), 0.0).carry();
        assert!(
            (carry - club.carry_m).abs() < club.carry_m * 0.05,
            "{}: {carry:.1} m, table {} m",
            club.name,
            club.carry_m
        );
    }
}

#[test]
fn clubs_are_ordered_and_power_scales_the_carry() {
    let carries: Vec<f32> = (0..CLUBS.len())
        .map(|i| shot(i, 1.0, Wind::default(), 0.0).carry())
        .collect();
    assert!(carries.windows(2).all(|w| w[0] > w[1]), "{carries:?}");
    let mut last = 0.0;
    for p in [0.2, 0.4, 0.6, 0.8, 1.0, 1.1] {
        let c = shot(0, p, Wind::default(), 0.0).carry();
        assert!(c > last, "power {p}: {c} after {last}");
        last = c;
    }
}

#[test]
fn a_shot_is_a_plausible_arc() {
    let f = shot(0, 1.0, Wind::default(), 0.0);
    let apex = f.event(Event::Apex).expect("an apex");
    let land = f.event(Event::Land).expect("a landing");
    assert!(apex.t > 2.0 && apex.t < land.t);
    assert!(
        (15.0..45.0).contains(&f.apex_height()),
        "{}",
        f.apex_height()
    );
    assert!((4.0..9.0).contains(&f.duration()), "{}", f.duration());
    assert_eq!(land.position.y, 0.0);
    assert!(f.samples.iter().all(|s| s.position.y >= 0.0));
    // The ball interpolates between samples and clamps at both ends.
    assert_eq!(f.at(-1.0), f.samples[0].position);
    assert_eq!(f.at(99.0), land.position);
    let mid = f.at(f.duration() * 0.5);
    assert!(mid.x > 0.0 && mid.y > 10.0);
}

#[test]
fn the_same_shot_gives_the_same_samples() {
    let wind = Wind::blowing(Vec2::new(0.0, 1.0), 4.0);
    assert_eq!(shot(3, 0.9, wind, 300.0), shot(3, 0.9, wind, 300.0));
}

#[test]
fn wind_pushes_downwind_and_head_wind_costs_distance() {
    let calm = shot(0, 1.0, Wind::default(), 0.0).carry();
    let tail = shot(0, 1.0, Wind::blowing(EAST, 6.0), 0.0).carry();
    let head = shot(0, 1.0, Wind::blowing(Vec2::new(-1.0, 0.0), 6.0), 0.0).carry();
    assert!(
        tail > calm + 3.0 && head < calm - 3.0,
        "{head} {calm} {tail}"
    );
    let cross = shot(0, 1.0, Wind::blowing(Vec2::new(0.0, 1.0), 6.0), 0.0);
    assert!(cross.at(cross.duration()).z > 3.0, "blown south");
}

#[test]
fn sidespin_curves_the_ball_and_backspin_lifts_it() {
    let straight = shot(0, 1.0, Wind::default(), 0.0);
    let end = |f: &Flight| f.at(f.duration());
    assert!(end(&straight).z.abs() < 0.5, "no spin, no curve");
    // Heading east, right is south (+z).
    let slice = shot(0, 1.0, Wind::default(), 1500.0);
    let hook = shot(0, 1.0, Wind::default(), -1500.0);
    assert!(end(&slice).z > 8.0, "slice goes right: {}", end(&slice).z);
    assert!(end(&hook).z < -8.0, "hook goes left: {}", end(&hook).z);
    assert!((end(&slice).z + end(&hook).z).abs() < 1.0, "symmetric");
    // The same launch with no spin falls much sooner.
    let mut launch = CLUBS[0].launch(Vec3::default(), EAST, 1.0, 0.0);
    launch.backspin = 0.0;
    let dead = simulate(&launch, Wind::default(), flat);
    assert!(dead.apex_height() < straight.apex_height() - 5.0);
}

#[test]
fn the_ball_lands_on_rising_and_falling_ground() {
    // A ramp rising 1 m per 10 m: the ball lands on it, at its height.
    let ramp = |x: f32, _: f32| x * 0.1;
    let launch = CLUBS[6].launch(Vec3::new(0.0, 0.0, 0.0), EAST, 1.0, 0.0);
    let up = simulate(&launch, Wind::default(), ramp);
    let land = up.event(Event::Land).unwrap();
    assert!((land.position.y - land.position.x * 0.1).abs() < 0.05);
    let down = simulate(&launch, Wind::default(), |x, _| -x * 0.1);
    assert!(down.carry() > up.carry() + 10.0, "downhill carries further");
    // A launch from below the ground ends at once and never loops.
    let buried = simulate(&launch, Wind::default(), |_, _| 100.0);
    assert!(buried.samples.len() <= 3);
}

// -- the ball on screen --------------------------------------------------

use xui_golf::render::overlay::{draw, BallMark, Overlay};
use xui_golf::render::palette::{BLACK, WHITE};
use xui_golf::render::{Camera, Frame, Mode, RenderJob, Scene};
use xui_golf::Params;

const W: usize = 640;
const H: usize = 360;

fn count(frame: &Frame, index: u8) -> usize {
    frame.index.iter().filter(|&&i| i == index).count()
}

#[test]
fn the_ball_is_depth_tested_and_clipped() {
    let camera = Camera::new(Vec3::new(0.0, 2.0, 0.0), 0.0, 0.0);
    let view = camera.view(W, H);
    let ball = Overlay {
        ball: Some(BallMark {
            position: Vec3::new(0.0, 2.0, -50.0),
            ground: 0.0,
        }),
        ..Overlay::default()
    };
    // Open air: a small white disc at the centre, and a shadow below it.
    let mut frame = Frame::new(W, H);
    draw(&mut frame, &view, &ball);
    let white = count(&frame, WHITE);
    assert!((1..=40).contains(&white), "{white} white pixels");
    assert_eq!(frame.index[(H / 2) * W + W / 2], WHITE);
    assert!(count(&frame, BLACK) > 0, "a shadow on the ground");

    // Something nearer covers it: nothing is drawn where it stands.
    let mut frame = Frame::new(W, H);
    frame.depth.iter_mut().for_each(|d| *d = 1.0 / 20.0);
    draw(&mut frame, &view, &ball);
    assert_eq!(count(&frame, WHITE), 0, "hidden behind a nearer surface");

    // Behind the camera, and far off screen, draw nothing and never panic.
    for position in [
        Vec3::new(0.0, 2.0, 50.0),
        Vec3::new(9000.0, 2.0, -50.0),
        Vec3::new(0.0, 2.0, -5000.0),
    ] {
        let mut frame = Frame::new(W, H);
        let overlay = Overlay {
            ball: Some(BallMark {
                position,
                ground: 0.0,
            }),
            ..Overlay::default()
        };
        draw(&mut frame, &view, &overlay);
        assert_eq!(count(&frame, WHITE), 0);
    }
}

/// A driver off hole 1's tee, the ball high over the fairway, with its path
/// stamped as a dotted trail: left in `target/snapshots/golf-ball-*.png`.
#[test]
fn a_ball_in_the_air_over_a_course() {
    let scene = Scene::new(xui_golf::generate(Params::quick(1)));
    let hole = &scene.course.holes[0];
    let heading = hole.tee_heading();
    let tee = hole.tees[0].position;
    let ground = |x: f32, z: f32| scene.bake.height_at(x, z);
    let start = Vec3::new(tee.x, ground(tee.x, tee.y) + 0.03, tee.y);
    let launch = CLUBS[0].launch(start, heading, 1.0, 0.0);
    let flight = simulate(&launch, Wind::default(), ground);
    assert!(flight.carry() > 150.0);

    // Behind the tee, looking at the point 60% along the flight.
    let eye_xz = tee - heading * 7.0;
    let eye = Vec3::new(eye_xz.x, ground(eye_xz.x, eye_xz.y) + 2.5, eye_xz.y);
    let aim = flight.at(flight.duration() * 0.6);
    let flat = Vec2::new(aim.x - eye.x, aim.z - eye.z);
    let camera = Camera::new(
        eye,
        flat.x.atan2(-flat.y),
        ((aim.y - eye.y) / flat.length()).atan() * 0.5,
    );

    let mut frame = Frame::new(W, H);
    let now = flight.duration() * 0.6;
    let mark = |t: f32| {
        let p = flight.at(t);
        Overlay {
            ball: Some(BallMark {
                position: p,
                ground: scene.bake.height_at(p.x, p.z),
            }),
            ..Overlay::default()
        }
    };
    let mut job = RenderJob::new(&scene, &camera, W, H, Mode::Fly, 0.0).with_overlay(mark(now));
    job.step(&mut frame, &scene, None);
    // The ball is where the projection says, and white.
    let c = job.view().to_camera(flight.at(now));
    let (sx, sy) = job.view().project(c);
    let at = sy as usize * W + sx as usize;
    assert_eq!(frame.index[at], WHITE, "ball at ({sx:.0}, {sy:.0})");
    // The trail: one dot every quarter second before it.
    let mut t = 0.0;
    while t < now {
        draw(&mut frame, job.view(), &mark(t));
        t += 0.25;
    }
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let mut rgba = Vec::new();
    xui_golf::render::resolve_into(&frame, &scene.palette.words(), 2, W * 2, H * 2, &mut rgba);
    xui_core::Image::from_rgba((W * 2) as u32, (H * 2) as u32, rgba)
        .unwrap()
        .save_png(dir.join("golf-ball-flight.png"))
        .unwrap();
}

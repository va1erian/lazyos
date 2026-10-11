//! The address camera's promises, held for every hole of several courses
//! and for balls along the line of play: the ball and the aim point are in
//! the picture, the eye is above the ground and outside the trees, and the
//! ball can be seen (except in the few places where it cannot).

use xui_golf::course::ObjectKind;
use xui_golf::math::{polyline_at, Vec2, Vec3};
use xui_golf::play::cam::{Obstacles, MIN_CLEARANCE};
use xui_golf::play::{caddy, Play};
use xui_golf::render::Scene;
use xui_golf::{Archetype, Params};

const ASPECTS: [f32; 2] = [9.0 / 16.0, 3.0 / 4.0];

fn scenes() -> Vec<Scene> {
    [
        (1, Archetype::Parkland),
        (2, Archetype::Links),
        (3, Archetype::Mountain),
        (7, Archetype::Parkland),
        (8, Archetype::Mountain),
    ]
    .into_iter()
    .map(|(seed, a)| Scene::new(xui_golf::generate(Params::quick(seed).with_archetype(a))))
    .collect()
}

/// A ball on hole `n`'s centerline `fraction` of the way to the green.
fn lie(scene: &Scene, n: usize, fraction: f32) -> Play {
    let hole = &scene.course.holes[n];
    if fraction <= 0.0 {
        return Play::tee_off(&scene.course.holes, &scene.bake, n);
    }
    let (p, _) = polyline_at(&hole.centerline, hole.length_m * fraction);
    let ball = Vec3::new(p.x, scene.bake.height_at(p.x, p.y) + 0.03, p.y);
    let (heading, distance) = caddy::default_aim(hole, p);
    let mut play = Play::tee_off(&scene.course.holes, &scene.bake, n);
    play.ball = ball;
    play.heading = heading;
    play.club = caddy::recommend(distance, false);
    play.distance = distance.min(play.carry());
    play
}

/// (x, y) in 0..1 of the picture, or `None` behind the camera.
fn on_screen(cam: &xui_golf::render::Camera, aspect: f32, p: Vec3) -> Option<(f32, f32)> {
    let (w, h) = (1000usize, (1000.0 * aspect) as usize);
    let view = cam.view(w, h);
    let c = view.to_camera(p);
    if c.z < 0.5 {
        return None;
    }
    let (x, y) = view.project(c);
    Some((x / w as f32, y / h as f32))
}

#[test]
fn the_address_camera_frames_every_shot() {
    let (mut shots, mut blind) = (0, 0);
    for scene in scenes() {
        let world = Obstacles { bake: &scene.bake };
        for n in 0..scene.course.holes.len() {
            for fraction in [0.0, 0.35, 0.65, 0.9] {
                let play = lie(&scene, n, fraction);
                let aim = play.aim_point(&scene.bake);
                for aspect in ASPECTS {
                    let a = play.address_camera(&scene.bake, aspect);
                    let cam = a.camera;
                    let label = format!(
                        "seed {} hole {} at {fraction} aspect {aspect:.2}",
                        scene.course.seed,
                        n + 1
                    );
                    shots += 1;
                    blind += usize::from(a.blind);

                    // Above the ground, never inside a tree.
                    let ground = scene.bake.height_at(cam.eye.x, cam.eye.z);
                    assert!(
                        cam.eye.y >= ground + MIN_CLEARANCE - 1e-3,
                        "{label}: eye {:.2} m over the ground",
                        cam.eye.y - ground
                    );
                    if !a.blind {
                        assert!(!world.inside_tree(cam.eye), "{label}: eye in a tree");
                        let top = Vec3::new(play.ball.x, play.ball.y + 0.1, play.ball.z);
                        assert!(!world.blocked(cam.eye, top, 1.5), "{label}: ball hidden");
                    }

                    // The ball and the aim marker are in the picture.
                    for (what, p) in [("ball", play.ball), ("aim", aim)] {
                        let (x, y) = on_screen(&cam, aspect, p)
                            .unwrap_or_else(|| panic!("{label}: {what} behind the camera"));
                        assert!(
                            (0.0..=1.0).contains(&x) && (0.03..=0.97).contains(&y),
                            "{label}: {what} at ({x:.2}, {y:.2})"
                        );
                    }
                    // The ball sits in the lower half of the picture.
                    let ball = on_screen(&cam, aspect, play.ball).unwrap();
                    assert!(ball.1 >= 0.5, "{label}: ball at {:.2}", ball.1);
                }
            }
        }
    }
    let share = blind as f32 / shots as f32;
    eprintln!("{shots} shots, {blind} blind ({:.1}%)", share * 100.0);
    assert!(share < 0.05, "too many blind shots: {blind} of {shots}");
}

#[test]
fn aiming_and_clubs_stay_valid() {
    let scene = &scenes()[0];
    let mut play = lie(scene, 0, 0.0);
    let start = play.heading;
    for _ in 0..360 {
        play.rotate(0.1f32.to_radians() * 10.0);
        assert!((play.heading.length() - 1.0).abs() < 1e-4);
    }
    // A full turn comes back.
    assert!((play.heading.x - start.x).abs() < 1e-2 && (play.heading.y - start.y).abs() < 1e-2);
    // A quarter turn clockwise from east points south (+z).
    play.heading = Vec2::new(1.0, 0.0);
    play.rotate(std::f32::consts::FRAC_PI_2);
    assert!(play.heading.y > 0.99);
    play.step_club(-100);
    assert_eq!(play.club, 0);
    play.step_club(100);
    assert_eq!(
        play.club,
        xui_golf::play::BAG.len() - 1,
        "the putter is last"
    );
    // The camera follows the aim: turning it right turns the camera right.
    let a = lie(scene, 0, 0.0);
    let mut b = a.clone();
    b.rotate(0.4);
    let (ya, yb) = (
        a.camera(&scene.bake, 0.5625).yaw,
        b.camera(&scene.bake, 0.5625).yaw,
    );
    assert!((yb - ya - 0.4).abs() < 0.15, "yaw {ya} -> {yb}");
}

#[test]
fn trees_are_the_only_blockers_and_the_course_has_some() {
    let scene = &scenes()[0];
    let trees = scene
        .bake
        .objects
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Tree { .. }))
        .count();
    assert!(trees > 100, "{trees} trees");
    let near = scene
        .bake
        .objects_near(Vec2::new(512.0, 512.0), 80.0)
        .count();
    let all = scene
        .bake
        .objects
        .iter()
        .filter(|o| o.position.distance(Vec2::new(512.0, 512.0)) <= 80.0)
        .count();
    assert_eq!(
        near, all,
        "the chunk lookup finds exactly the objects in range"
    );
}

/// The address view as the player sees it, for a few tees and approaches:
/// `target/snapshots/golf-address-*.png`, for a human to look at.
#[test]
fn address_views_for_a_few_shots() {
    use xui_golf::render::{resolve_into, AimMark, BallMark, Frame, Mode, Overlay, RenderJob};
    const W: usize = 640;
    const H: usize = 360;
    let scenes = scenes();
    let cases = [
        (0, 0, 0.0),
        (0, 8, 0.0),
        (1, 4, 0.0),
        (2, 11, 0.0),
        (0, 2, 0.7),
        (3, 6, 0.8),
    ];
    for (si, hole, fraction) in cases {
        let scene = &scenes[si];
        let play = lie(scene, hole, fraction);
        let a = play.address_camera(&scene.bake, H as f32 / W as f32);
        let mark: AimMark = play.aim_mark(&scene.bake);
        let overlay = Overlay {
            ball: Some(BallMark {
                position: play.ball,
                ground: scene.bake.height_at(play.ball.x, play.ball.z),
            }),
            aim: Some(mark),
            grid: Vec::new(),
        };
        let mut frame = Frame::new(W, H);
        let mut job = RenderJob::new(scene, &a.camera, W, H, Mode::Fly, 0.0).with_overlay(overlay);
        job.step(&mut frame, scene, None);
        let mut rgba = Vec::new();
        resolve_into(&frame, &scene.palette.words(), 2, W * 2, H * 2, &mut rgba);
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!(
            "golf-address-{si}-h{}-{}.png",
            hole + 1,
            (fraction * 100.0) as u32
        );
        xui_core::Image::from_rgba((W * 2) as u32, (H * 2) as u32, rgba)
            .unwrap()
            .save_png(dir.join(name))
            .unwrap();
    }
}

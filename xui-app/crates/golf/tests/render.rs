//! The renderer offscreen: views from a tee, down a fairway and from the
//! air, left in `target/snapshots/golf-view-*.png` for a human to look at,
//! with checks that they are full pictures and a timing per frame.

use std::path::PathBuf;
use std::time::Instant;

use xui_golf::math::Vec3;
use xui_golf::render::{resolve_into, Camera, Frame, Mode, RenderJob, Scene};
use xui_golf::{Archetype, Params};

const W: usize = 640;
const H: usize = 360;

fn scene(seed: u64, archetype: Archetype) -> Scene {
    Scene::new(xui_golf::generate(
        Params::quick(seed).with_archetype(archetype),
    ))
}

/// Behind hole `n`'s back tee at eye height, looking down the first leg.
fn tee_camera(scene: &Scene, n: usize, back: f32, up: f32) -> Camera {
    let hole = &scene.course.holes[n];
    let heading = hole.tee_heading();
    let p = hole.tees[0].position - heading * back;
    let ground = scene.bake.height_at(p.x, p.y);
    let yaw = heading.x.atan2(-heading.y);
    Camera::new(Vec3::new(p.x, ground + up, p.y), yaw, -(up / 260.0).atan())
}

/// On hole `n`'s centerline `along` metres from the tee, looking along it.
fn fairway_camera(scene: &Scene, n: usize, along: f32) -> Camera {
    let hole = &scene.course.holes[n];
    let (p, dir) = xui_golf::math::polyline_at(&hole.centerline, along);
    let ground = scene.bake.height_at(p.x, p.y);
    Camera::new(
        Vec3::new(p.x, ground + 1.7, p.y),
        dir.x.atan2(-dir.y),
        -0.02,
    )
}

fn save(scene: &Scene, frame: &Frame, name: &str) {
    let mut rgba = Vec::new();
    resolve_into(frame, &scene.palette.words(), 2, W * 2, H * 2, &mut rgba);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let image = xui_core::Image::from_rgba((W * 2) as u32, (H * 2) as u32, rgba).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The index buffer expands nearest-neighbour and crops, into a buffer that
/// is written over (not appended to) whatever size it had.
#[test]
fn resolve_into_scales_crops_and_reuses_the_buffer() {
    let mut frame = Frame::new(2, 2);
    frame.index.copy_from_slice(&[1, 2, 3, 4]);
    let mut words = [0u32; 256];
    for (i, w) in words.iter_mut().enumerate() {
        *w = u32::from_le_bytes([i as u8, 0, 0, 255]);
    }
    let reds = |rgba: &[u8]| rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>();

    // 2x scale, cropped to 3x3 (an odd window): the last column and row
    // are cut off the 4x4 expansion.
    let mut out = vec![0xEE; 1000];
    resolve_into(&frame, &words, 2, 3, 3, &mut out);
    assert_eq!(out.len(), 3 * 3 * 4);
    assert_eq!(reds(&out), [1, 1, 2, 1, 1, 2, 3, 3, 4]);
    assert!(out.chunks(4).all(|p| p[3] == 255));

    // A bigger window fills the same allocation, not the old bytes.
    out.reserve(8 * 8 * 4);
    let capacity = out.capacity();
    resolve_into(&frame, &words, 4, 8, 8, &mut out);
    assert_eq!(out.capacity(), capacity, "no new allocation");
    assert_eq!(out.len(), 8 * 8 * 4);
    assert_eq!(out[0], 1);
    assert_eq!(out[(7 * 8 + 7) * 4], 4);
}

/// How many distinct palette indices a frame uses.
fn colours(frame: &Frame) -> usize {
    let mut seen = [false; 256];
    frame
        .index
        .iter()
        .for_each(|&i| seen[usize::from(i)] = true);
    seen.iter().filter(|&&s| s).count()
}

#[test]
fn views_render_and_report_their_cost() {
    for (seed, archetype) in [
        (1, Archetype::Parkland),
        (2, Archetype::Links),
        (3, Archetype::Mountain),
    ] {
        let scene = scene(seed, archetype);
        let mut frame = Frame::new(W, H);
        let views = [
            ("tee", tee_camera(&scene, 0, 4.0, 1.7)),
            ("fairway", fairway_camera(&scene, 3, 150.0)),
            ("air", tee_camera(&scene, 6, 120.0, 60.0)),
        ];
        for (name, camera) in views {
            let start = Instant::now();
            let mut job = RenderJob::new(&scene, &camera, W, H, Mode::Fly, 0.0);
            job.step(&mut frame, &scene, None);
            let took = start.elapsed();
            eprintln!(
                "seed {seed} {name}: {took:?} for {} chunks, {} colours",
                job.chunks(),
                colours(&frame)
            );
            save(&scene, &frame, &format!("golf-view-{seed}-{name}.png"));
            // A fairway view into a dip can be mostly two mowing bands.
            assert!(colours(&frame) > 25, "{name}: a flat picture");
            // Sky at the top, ground at the bottom.
            assert!(frame.depth[W / 2] == 0.0 || name == "air");
            assert!(
                frame.depth[(H - 1) * W + W / 2] > 0.0,
                "{name}: no ground underfoot"
            );
        }
    }
}

#[test]
fn authentic_mode_paints_progressively_to_the_same_picture() {
    let scene = scene(5, Archetype::Parkland);
    let camera = tee_camera(&scene, 0, 4.0, 1.7);
    let mut fast = Frame::new(W, H);
    scene.render(&camera, &mut fast, 0.0);
    let mut slow = Frame::new(W, H);
    let mut job = RenderJob::new(&scene, &camera, W, H, Mode::Authentic { chunks: 2 }, 0.0);
    let mut steps = 0;
    while !job.step(&mut slow, &scene, None) {
        steps += 1;
    }
    assert!(steps > 10, "{steps}");
    let same = fast
        .index
        .iter()
        .zip(&slow.index)
        .filter(|(a, b)| a == b)
        .count();
    assert!(same as f32 > 0.97 * (W * H) as f32, "{same}");
}

/// Creeping forward 5 cm a frame, the middle distance (50-200 m) must hold
/// still: a pattern finer than a pixel's footprint there lands on a new cell
/// with every small move and sparkles (12-19% of those pixels changed per
/// frame before patterns faded with distance; about 5% now).
#[test]
fn a_creeping_camera_does_not_sparkle() {
    let scene = scene(2, Archetype::Parkland);
    let hole = &scene.course.holes[2];
    let dir = hole.tee_heading();
    let mut frame = Frame::new(W, H);
    let mut previous: Option<Vec<u8>> = None;
    let (mut changed, mut seen) = (0usize, 0usize);
    for k in 0..20 {
        let p = hole.tees[0].position + dir * (10.0 + k as f32 * 0.05);
        let ground = scene.bake.height_at(p.x, p.y);
        let camera = Camera::new(
            Vec3::new(p.x, ground + 1.7, p.y),
            dir.x.atan2(-dir.y),
            -0.05,
        );
        scene.render(&camera, &mut frame, 0.0);
        if let Some(previous) = &previous {
            for (i, (a, b)) in previous.iter().zip(&frame.index).enumerate() {
                let iz = frame.depth[i];
                if iz > 0.0 && (50.0..200.0).contains(&(1.0 / iz)) {
                    seen += 1;
                    changed += usize::from(a != b);
                }
            }
        }
        previous = Some(frame.index.clone());
    }
    let share = changed as f32 / seen.max(1) as f32;
    eprintln!(
        "middle distance: {:.1}% of pixels change per frame",
        share * 100.0
    );
    assert!(seen > 1000, "no middle distance in view");
    assert!(share < 0.08, "the middle distance sparkles: {share:.3}");
}

/// Searched courses from the seeds the app starts with, three tee views and
/// the map each, for a human to judge (slow:
/// `cargo test -p xui-golf --test render searched_views -- --ignored`).
#[test]
#[ignore]
fn searched_views() {
    for seed in 1..=3 {
        let scene = Scene::new(xui_golf::generate(Params::new(seed)));
        let course = &scene.course;
        eprintln!(
            "seed {seed}: {} {} quality {:.2}",
            course.name,
            course.archetype.name(),
            course.quality
        );
        let mut frame = Frame::new(W, H);
        for n in [0, 4, 9] {
            scene.render(&tee_camera(&scene, n, 4.0, 1.7), &mut frame, 0.0);
            save(
                &scene,
                &frame,
                &format!("golf-search-{seed}-tee{}.png", n + 1),
            );
        }
        scene.render(&tee_camera(&scene, 12, 160.0, 90.0), &mut frame, 0.0);
        save(&scene, &frame, &format!("golf-search-{seed}-air.png"));
        let map = xui_golf::minimap::Minimap::new(course, 512);
        let image = xui_core::Image::from_rgba(512, 512, map.rgba).unwrap();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
        image
            .save_png(dir.join(format!("golf-search-{seed}-map.png")))
            .unwrap();
    }
}

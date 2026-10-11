//! Playing holes through the real state machine: a computer player that
//! clicks the meter at the tick, the camera director following every
//! shot, the rules and the score.

use xui_golf::math::Vec3;
use xui_golf::play::{Phase, Play, PlayEvent, PUTTER_INDEX};
use xui_golf::render::{Camera, Scene};
use xui_golf::{Archetype, Params};

const ASPECT: f32 = 9.0 / 16.0;
const DT: f32 = 1.0 / 30.0;

fn scenes() -> Vec<Scene> {
    [
        (1, Archetype::Parkland),
        (2, Archetype::Links),
        (3, Archetype::Mountain),
    ]
    .into_iter()
    .map(|(seed, a)| Scene::new(xui_golf::generate(Params::quick(seed).with_archetype(a))))
    .collect()
}

fn on_screen(cam: &Camera, p: Vec3) -> Option<(f32, f32)> {
    let view = cam.view(1000, (1000.0 * ASPECT) as usize);
    let c = view.to_camera(p);
    if c.z < 0.3 {
        return None;
    }
    let (x, y) = view.project(c);
    Some((x / 1000.0, y / (1000.0 * ASPECT)))
}

#[derive(Default, Debug)]
struct Stats {
    shots: u32,
    frames: u32,
    ball_visible: u32,
    min_eye_clearance: f32,
    cuts: u32,
    penalties: u32,
    holed: u32,
    pickups: u32,
    putts: u32,
}

/// Plays hole `n` from the tee; returns the strokes.
fn play_hole(scene: &Scene, n: usize, stats: &mut Stats) -> u32 {
    let (course, bake) = (&scene.course, &scene.bake);
    let mut play = Play::tee_off(&course.holes, bake, n);
    play.seed = course.seed;
    play.set_up(course, bake);
    let mut guard = 0;
    while play.scores[n].is_none() {
        guard += 1;
        assert!(
            guard < 6000,
            "hole {} never finished: {:?}",
            n + 1,
            play.phase
        );
        match play.phase {
            Phase::Address => {
                if play.club == PUTTER_INDEX {
                    stats.putts += 1;
                }
                let power = play.target_power(course, bake);
                play.hit(course, bake, ASPECT, power, 0.0);
                stats.shots += 1;
                stats.cuts += play.director.as_ref().map_or(0, |d| d.cuts().len() as u32);
            }
            _ => {
                play.tick(DT, course, bake, ASPECT);
                if let (Phase::Flight { .. }, Some(ball)) = (&play.phase, play.ball_shown()) {
                    let cam = play.camera(bake, ASPECT);
                    stats.frames += 1;
                    let ground = bake.height_at(cam.eye.x, cam.eye.z);
                    stats.min_eye_clearance = stats.min_eye_clearance.min(cam.eye.y - ground);
                    // In the picture, and not behind a rise or a tree.
                    let world = xui_golf::play::cam::Obstacles { bake };
                    let hidden = world.blocked(cam.eye, ball, 0.5);
                    if !hidden
                        && on_screen(&cam, ball).is_some_and(|(x, y)| {
                            (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)
                        })
                    {
                        stats.ball_visible += 1;
                    }
                }
            }
        }
        for e in play.events.drain(..) {
            if matches!(e, PlayEvent::Penalty { .. }) {
                stats.penalties += 1;
            }
        }
    }
    let strokes = play.scores[n].unwrap();
    if strokes >= xui_golf::play::MAX_STROKES {
        stats.pickups += 1;
    } else {
        stats.holed += 1;
    }
    strokes
}

#[test]
fn holes_are_played_to_the_cup_with_the_ball_in_view() {
    let mut stats = Stats {
        min_eye_clearance: f32::MAX,
        ..Stats::default()
    };
    let mut total = 0;
    let mut holes = 0;
    for scene in scenes() {
        for n in [0usize, 3, 6, 9, 12, 15] {
            let strokes = play_hole(&scene, n, &mut stats);
            let par = u32::from(scene.course.holes[n].par);
            eprintln!(
                "seed {} hole {:2} par {par}: {strokes} strokes",
                scene.course.seed,
                n + 1
            );
            total += strokes;
            holes += 1;
        }
    }
    eprintln!("{stats:?}\n{holes} holes, {total} strokes");
    // A perfect-contact player (always dead straight, at the tick) holes
    // out, or is picked up only rarely; in neither case does the game stop.
    assert!(
        stats.holed * 100 >= holes * 70,
        "only {} of {holes} holed",
        stats.holed
    );
    assert!(stats.putts > 0 && stats.cuts > 0, "putts and cuts happened");
    // The ball stays in the picture for nearly all of every flight.
    let share = stats.ball_visible as f32 / stats.frames as f32;
    assert!(
        share > 0.9,
        "the ball was in view {:.0}% of frames",
        share * 100.0
    );
    assert!(
        stats.min_eye_clearance > 0.3,
        "the camera touched the ground: {}",
        stats.min_eye_clearance
    );
}

#[test]
#[ignore = "trace one hole: TRACE_SCENE=<0..2> TRACE_HOLE=<0..17>"]
fn trace_one() {
    use xui_golf::play::terrain::{CourseTerrain, Terrain};
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let scenes = scenes();
    let scene = &scenes[env("TRACE_SCENE", 0)];
    let n = env("TRACE_HOLE", 3);
    let (course, bake) = (&scene.course, &scene.bake);
    let hole = &course.holes[n];
    let terrain = CourseTerrain::new(course, bake, n);
    eprintln!(
        "hole {} par {} len {:.0} pin ({:.0},{:.0})",
        n + 1,
        hole.par,
        hole.length_m,
        hole.pin.x,
        hole.pin.y
    );
    let mut play = Play::tee_off(&course.holes, bake, n);
    play.seed = course.seed;
    play.set_up(course, bake);
    let mut guard = 0;
    while play.scores[n].is_none() && guard < 6000 {
        guard += 1;
        if matches!(play.phase, Phase::Address) {
            let m = terrain.material(play.ball.x, play.ball.z);
            let p = play.target_power(course, bake);
            eprint!(
                "  #{} {:>9} from {:10} aim {:3.0} m (pin {:3.0}) power {:.2}",
                play.stroke + 1,
                play.club().name,
                m.name(),
                play.distance,
                play.distance_to_pin(&course.holes),
                p
            );
            play.hit(course, bake, ASPECT, p, 0.0);
            let f = play.flight.as_ref().unwrap();
            let end = f.samples.last().unwrap().position;
            let land = f.event(xui_golf::play::Event::Land).map(|s| s.position);
            eprintln!(
                " -> carry {:3.0} lands {:10} ends {:?} {} {:.0} m, {:.0} s",
                f.carry(),
                land.map_or("-", |l| terrain.material(l.x, l.z).name()),
                f.end(),
                terrain.material(end.x, end.z).name(),
                f.total(),
                f.duration()
            );
        } else {
            play.tick(DT, course, bake, ASPECT);
        }
        play.events.clear();
    }
    eprintln!("  holed in {:?}", play.scores[n]);
}

/// The picture the game draws for `play` right now.
fn picture(scene: &Scene, play: &Play, name: &str) {
    use xui_golf::play::green;
    use xui_golf::render::{resolve_into, BallMark, Frame, Mode, Overlay, RenderJob};
    const W: usize = 640;
    const H: usize = 360;
    let bake = &scene.bake;
    let camera = play.camera(bake, ASPECT);
    let lining = matches!(play.phase, Phase::Address | Phase::Swing(_));
    let mut overlay = Overlay {
        ball: play.ball_shown().map(|p| BallMark {
            position: p,
            ground: bake.height_at(p.x, p.z),
        }),
        ..Overlay::default()
    };
    if lining {
        overlay.aim = Some(play.aim_mark(bake));
        if play.club == PUTTER_INDEX {
            overlay.grid = green::grid(&scene.course.holes[play.hole], bake);
        }
    }
    let mut frame = Frame::new(W, H);
    let mut job = RenderJob::new(scene, &camera, W, H, Mode::Fly, 0.0).with_overlay(overlay);
    job.step(&mut frame, scene, None);
    if let Some(p) = play.ball_shown() {
        let v = job.view();
        let c = v.to_camera(p);
        let (x, y) = v.project(c);
        let (xi, yi) = (x as i64, y as i64);
        let white = (0..frame.width * frame.height)
            .filter(|&i| frame.index[i] == xui_golf::render::palette::WHITE)
            .count();
        if name.contains("landing") {
            for dy in -3i64..=3 {
                let row: Vec<String> = (-4i64..=4)
                    .map(|dx| {
                        let (px, py) = (xi + dx, yi + dy);
                        let i = py as usize * W + px as usize;
                        format!("{:3}/{:.4}", frame.index[i], frame.depth[i])
                    })
                    .collect();
                eprintln!("   {}", row.join(" "));
            }
            eprintln!("   ball iz would be ~{:.4} (z {:.1})", 1.0 / c.z, c.z);
        }
        eprintln!(
            "{name}: ball {:.0} m away at ({x:.0}, {y:.0}) of {W}x{H}, pixel {:?}, {white} white pixels, eye y {:.1} yaw {:.2} pitch {:.2} fov {:.0}",
            c.z,
            if (0..W as i64).contains(&xi) && (0..H as i64).contains(&yi) {
                Some(frame.index[yi as usize * W + xi as usize])
            } else {
                None
            },
            camera.eye.y,
            camera.yaw,
            camera.pitch,
            camera.fov.to_degrees()
        );
    }
    let mut rgba = Vec::new();
    resolve_into(&frame, &scene.palette.words(), 2, W * 2, H * 2, &mut rgba);
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    xui_core::Image::from_rgba((W * 2) as u32, (H * 2) as u32, rgba)
        .unwrap()
        .save_png(dir.join(name))
        .unwrap();
}

/// A drive and its landing cut, then a putt with the slope grid:
/// `target/snapshots/golf-play-*.png`, for a human to look at.
#[test]
fn flight_cameras_and_the_putting_view() {
    let scenes = scenes();
    let scene = &scenes[0];
    let (course, bake) = (&scene.course, &scene.bake);
    let n = 6;
    let mut play = Play::tee_off(&course.holes, bake, n);
    play.seed = course.seed;
    play.set_up(course, bake);
    picture(scene, &play, "golf-play-0-address.png");
    let power = play.target_power(course, bake);
    play.hit(course, bake, ASPECT, power, 0.0);
    let flight = play.flight.clone().unwrap();
    let apex = flight.event(xui_golf::play::Event::Apex).unwrap().t;
    let land = flight.event(xui_golf::play::Event::Land).unwrap().t;
    let marks = [
        ("1-climb", apex * 0.5),
        ("2-after-cut", apex + 0.3),
        ("3-landing", land + 0.2),
        ("4-roll", land + (flight.duration() - land) * 0.5),
    ];
    let mut t = 0.0;
    for (name, at) in marks {
        while t < at && matches!(play.phase, Phase::Flight { .. }) {
            play.tick(DT, course, bake, ASPECT);
            t += DT;
        }
        picture(scene, &play, &format!("golf-play-{name}.png"));
    }
    // To the rest, then the next address: a putt on this green.
    play.skip(course, bake);
    assert!(matches!(play.phase, Phase::Address), "{:?}", play.phase);
    picture(scene, &play, "golf-play-5-next-address.png");
    // A putt from 9 m, on the green's far side from the tee.
    let hole = &course.holes[n];
    let away = (hole.green_center - hole.pin).normalized();
    let lie = hole.pin + (hole.axis() * -1.0 + away * 0.5).normalized() * 9.0;
    let mut putt = Play::tee_off(&course.holes, bake, n);
    putt.ball = Vec3::new(lie.x, bake.height_at(lie.x, lie.y) + 0.03, lie.y);
    putt.stroke = 2;
    putt.seed = course.seed;
    putt.set_up(course, bake);
    assert_eq!(putt.club, PUTTER_INDEX, "on the green: the putter");
    picture(scene, &putt, "golf-play-6-putt.png");
    // And rolled: the camera follows it into the cup or past it.
    let power = putt.target_power(course, bake);
    putt.hit(course, bake, ASPECT, power, 0.0);
    let dur = putt.flight.as_ref().unwrap().duration();
    let mut t = 0.0;
    while t < dur * 0.6 {
        putt.tick(DT, course, bake, ASPECT);
        t += DT;
    }
    picture(scene, &putt, "golf-play-7-putt-roll.png");
}

/// A click that skips a shot still reports where the ball came down, once.
#[test]
fn skipping_a_flight_still_reports_the_landing() {
    let scenes = scenes();
    let scene = &scenes[0];
    let (course, bake) = (&scene.course, &scene.bake);
    let mut play = Play::tee_off(&course.holes, bake, 6);
    play.seed = course.seed;
    play.set_up(course, bake);
    let power = play.target_power(course, bake);
    play.hit(course, bake, ASPECT, power, 0.0);
    // The ball is still in the air.
    play.tick(DT, course, bake, ASPECT);
    assert!(matches!(play.phase, Phase::Flight { landed: false, .. }));
    play.skip(course, bake);
    let landed = play
        .events
        .iter()
        .filter(|e| matches!(e, PlayEvent::Landed { .. }))
        .count();
    assert_eq!(landed, 1, "events: {:?}", play.events);
    assert!(play.message.is_some(), "and the player is told");
}

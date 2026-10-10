//! Whole courses from several seeds: 18 playable holes with sane pars, the
//! features where the layout put them, the same course from the same seed.
//! Each course's overhead map is left in `target/snapshots/golf-map-*.png`
//! for a human to look at.

use std::path::PathBuf;
use std::time::Instant;

use xui_golf::minimap::Minimap;
use xui_golf::{Archetype, Course, Generator, Material, ObjectKind, Params};

fn generate_timed(params: Params) -> Course {
    let mut generator = Generator::new(params);
    loop {
        let stage = generator.stage();
        let start = Instant::now();
        let done = generator.step();
        eprintln!("seed {} {:?}: {:?}", params.seed, stage, start.elapsed());
        if let Some(course) = done {
            return course;
        }
    }
}

fn save_map(course: &Course, name: &str) {
    let map = Minimap::new(course, 512);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let image = xui_core::Image::from_rgba(map.width as u32, map.height as u32, map.rgba).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

fn check(course: &Course) {
    assert_eq!(course.holes.len(), 18);
    let mut indexes: Vec<u8> = course.holes.iter().map(|h| h.handicap_index).collect();
    indexes.sort();
    assert_eq!(indexes, (1..=18).collect::<Vec<u8>>());
    for hole in &course.holes {
        let n = hole.number;
        eprintln!(
            "  hole {n:2} par {} {:4.0} m (eff {:4.0}) scratch {:.2} bogey {:.2} hcp {:2}",
            hole.par,
            hole.length_m,
            hole.effective_length_m,
            hole.scratch_expected,
            hole.bogey_expected,
            hole.handicap_index
        );
        assert!((3..=5).contains(&hole.par), "hole {n} par {}", hole.par);
        assert_eq!(
            course.material_at(hole.pin.x, hole.pin.y),
            Material::Green,
            "hole {n} pin {:?} green {:?} clubhouse {:?}",
            hole.pin,
            hole.green_center,
            course.clubhouse
        );
        let tee = hole.tees[0].position;
        assert_eq!(
            course.material_at(tee.x, tee.y),
            Material::TeeBox,
            "hole {n} tee"
        );
        assert!(hole.tees.len() >= 3);
        assert!(
            hole.scratch_expected > 2.0 && hole.scratch_expected < hole.bogey_expected + 0.01,
            "hole {n}: scratch {} bogey {}",
            hole.scratch_expected,
            hole.bogey_expected
        );
    }
    eprintln!(
        "{}: {} par {} {:.0} m rating {} slope {}",
        course.name,
        course.archetype.name(),
        course.par,
        course.total_length_m(),
        course.rating,
        course.slope
    );
    let trees = course
        .objects
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Tree { .. }))
        .count();
    let flags = course
        .objects
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Flagstick { .. }))
        .count();
    assert_eq!(flags, 18);
    assert!((66..=76).contains(&course.par), "par {}", course.par);
    eprintln!("trees {trees}");
}

#[test]
fn courses_from_three_seeds() {
    for (seed, archetype) in [
        (1, Archetype::Parkland),
        (2, Archetype::Links),
        (3, Archetype::Mountain),
    ] {
        let course = generate_timed(Params::quick(seed).with_archetype(archetype));
        save_map(&course, &format!("golf-map-{seed}.png"));
        check(&course);
    }
}

#[test]
fn the_search_picks_a_better_course_than_the_plain_seed() {
    let plain = xui_golf::generate(Params::quick(7));
    let searched = generate_timed(Params::new(7));
    save_map(&searched, "golf-map-search-7.png");
    check(&searched);
    eprintln!(
        "quality: plain {:.2}, searched {:.2}",
        plain.quality, searched.quality
    );
    assert!(
        searched.quality >= plain.quality,
        "the search kept a worse course"
    );
}

#[test]
fn the_same_seed_makes_the_same_course() {
    let params = Params::quick(42).with_archetype(Archetype::Parkland);
    let (a, b) = (xui_golf::generate(params), xui_golf::generate(params));
    assert_eq!(a.par, b.par);
    assert_eq!(a.objects.len(), b.objects.len());
    assert!(a.elevation == b.elevation && a.material == b.material);
}

/// How often the routing fits without its fallback, across seeds (slow:
/// `cargo test -p xui-golf --test generator routing_survey -- --ignored`;
/// `GOLF_SEEDS=1,2,3` picks the seeds).
#[test]
#[ignore]
fn routing_survey() {
    for archetype in Archetype::ALL {
        let mut levels = Vec::new();
        let seeds: Vec<u64> = match std::env::var("GOLF_SEEDS") {
            Ok(list) => list
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect(),
            Err(_) => (100..112).collect(),
        };
        for seed in seeds {
            let mut generator = Generator::new(Params::quick(seed).with_archetype(archetype));
            while generator.routing_relaxation().is_none() {
                generator.step();
            }
            levels.push(generator.routing_relaxation().unwrap());
        }
        eprintln!("{}: {:?}", archetype.name(), levels);
    }
}

/// Quality across seeds, plain against searched (slow:
/// `cargo test -p xui-golf --test generator quality_survey -- --ignored --nocapture`).
#[test]
#[ignore]
fn quality_survey() {
    let (mut plain_sum, mut search_sum, mut worst) = (0.0, 0.0, f32::MAX);
    let seeds: Vec<u64> = (200..210).collect();
    for &seed in &seeds {
        let start = Instant::now();
        let mut generator = Generator::new(Params::new(seed));
        while generator.routing_relaxation().is_none() {
            generator.step();
        }
        let searched = generator.quality();
        let took = start.elapsed();
        let mut generator = Generator::new(Params::quick(seed));
        while generator.routing_relaxation().is_none() {
            generator.step();
        }
        let plain = generator.quality();
        eprintln!(
            "seed {seed}: plain {:.2} searched {:.2} {searched:?} ({took:?})",
            plain.total, searched.total
        );
        plain_sum += plain.total;
        search_sum += searched.total;
        worst = worst.min(searched.total);
    }
    let n = seeds.len() as f32;
    eprintln!(
        "mean plain {:.2}, searched {:.2}, worst searched {worst:.2}",
        plain_sum / n,
        search_sum / n
    );
}

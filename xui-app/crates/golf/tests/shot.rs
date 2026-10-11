//! A whole shot on controlled ground: the bounce, the roll, the lie, the
//! cup, the water, the trees.

use xui_golf::course::Material;
use xui_golf::math::{Vec2, Vec3};
use xui_golf::play::ball::{Event, Flight, Wind};
use xui_golf::play::shot::play_out;
use xui_golf::play::terrain::{Plane, Terrain};
use xui_golf::play::{BAG, PUTTER_INDEX};

const EAST: Vec2 = Vec2::new(1.0, 0.0);

fn plane(material: Material) -> Plane {
    Plane {
        slope: Vec2::new(0.0, 0.0),
        material,
        pin: None,
    }
}

fn hit(club: usize, power: f32, terrain: &dyn Terrain) -> Flight {
    let at = Vec3::new(0.0, terrain.height(0.0, 0.0) + 0.02, 0.0);
    let launch = BAG[club].launch(at, EAST, power, 0.0);
    play_out(&launch, Wind::default(), terrain)
}

fn count(f: &Flight, e: Event) -> usize {
    f.samples.iter().filter(|s| s.event == e).count()
}

#[test]
fn a_drive_lands_bounces_and_comes_to_rest() {
    let f = hit(0, 1.0, &plane(Material::Fairway));
    assert_eq!(f.end(), Event::Rest);
    assert_eq!(count(&f, Event::Land), 1);
    assert!(count(&f, Event::Bounce) >= 1, "it hops");
    assert!(f.total() > f.carry() + 5.0, "and rolls on");
    assert!(f.total() < f.carry() + 60.0);
    assert!(f.duration() < 30.0, "{} s", f.duration());
    // Deterministic.
    assert_eq!(f, hit(0, 1.0, &plane(Material::Fairway)));
}

#[test]
fn the_surface_decides_how_far_it_runs() {
    let run = |m| {
        let f = hit(0, 1.0, &plane(m));
        f.total() - f.carry()
    };
    let (path, fairway, rough, deep, sand) = (
        run(Material::CartPath),
        run(Material::Fairway),
        run(Material::Rough),
        run(Material::DeepRough),
        run(Material::Sand),
    );
    assert!(
        path > fairway && fairway > rough && rough > deep && deep >= sand,
        "{path} {fairway} {rough} {deep} {sand}"
    );
    assert!(sand < 1.5, "sand plugs the ball: {sand}");
}

#[test]
fn a_wedge_on_a_green_checks_and_a_driver_does_not() {
    let green = plane(Material::Green);
    let wedge = hit(9, 1.0, &green);
    let driver = hit(0, 1.0, &green);
    let wedge_run = wedge.total() - wedge.carry();
    let driver_run = driver.total() - driver.carry();
    assert!(wedge_run < 8.0, "a wedge stops fast: {wedge_run}");
    assert!(driver_run > wedge_run + 10.0, "{driver_run}");
}

#[test]
fn a_bounce_never_gains_speed() {
    let f = hit(5, 1.0, &plane(Material::Fairway));
    for (i, s) in f.samples.iter().enumerate() {
        if s.event == Event::Bounce {
            let before = s.velocity.length();
            let after = f.samples[i + 1].velocity.length();
            assert!(after <= before + 1e-3, "{before} -> {after}");
        }
    }
}

#[test]
fn a_putt_rolls_the_distance_it_was_asked() {
    let green = plane(Material::Green);
    for distance in [3.0, 8.0, 15.0, 30.0] {
        let power = BAG[PUTTER_INDEX].power_for(distance);
        let f = hit(PUTTER_INDEX, power, &green);
        assert_eq!(f.end(), Event::Rest);
        assert_eq!(count(&f, Event::Land), 0, "a putt never leaves the ground");
        let rolled = f.total();
        assert!(
            (rolled - distance).abs() < distance * 0.06 + 0.1,
            "wanted {distance}, rolled {rolled}"
        );
    }
}

#[test]
fn a_putt_breaks_downhill() {
    // The ground falls toward -z: a putt along +x drifts to -z.
    let slope = Plane {
        slope: Vec2::new(0.0, 0.04),
        material: Material::Green,
        pin: None,
    };
    let power = BAG[PUTTER_INDEX].power_for(12.0);
    let f = hit(PUTTER_INDEX, power, &slope);
    let end = f.samples.last().unwrap().position;
    assert!(end.z < -0.5, "drifted {}", end.z);
    assert!(end.x > 6.0, "and still went forward: {}", end.x);
    // On a steep enough slope a ball at rest does not stay at rest.
    let steep = Plane {
        slope: Vec2::new(0.2, 0.0),
        material: Material::Green,
        pin: None,
    };
    let at = Vec3::new(0.0, 0.02, 0.0);
    let mut l = BAG[PUTTER_INDEX].launch(at, EAST, 0.0, 0.0);
    l.speed = 0.0;
    let f = play_out(&l, Wind::default(), &steep);
    assert!(f.total() > 1.0, "it ran down: {}", f.total());
}

#[test]
fn a_slow_putt_drops_and_a_fast_one_lips_past() {
    let cup = Plane {
        slope: Vec2::new(0.0, 0.0),
        material: Material::Green,
        pin: Some(Vec2::new(5.0, 0.0)),
    };
    let slow = hit(PUTTER_INDEX, BAG[PUTTER_INDEX].power_for(5.3), &cup);
    assert_eq!(slow.end(), Event::Holed);
    let end = slow.samples.last().unwrap().position;
    assert!((end.x - 5.0).abs() < 1e-3 && end.y < 0.0);
    let fast = hit(PUTTER_INDEX, 1.0, &cup);
    assert_eq!(fast.end(), Event::Rest, "too fast to drop");
    assert!(fast.total() > 6.0);
    // A short one that stops short stays short.
    let short = hit(PUTTER_INDEX, BAG[PUTTER_INDEX].power_for(3.0), &cup);
    assert_eq!(short.end(), Event::Rest);
}

/// Water from x = 150, out of bounds from x = 220.
struct Shore;

impl Terrain for Shore {
    fn height(&self, _: f32, _: f32) -> f32 {
        0.0
    }
    fn material(&self, x: f32, _: f32) -> Material {
        match x {
            x if x >= 220.0 => Material::OutOfBounds,
            x if x >= 150.0 => Material::Water,
            _ => Material::Fairway,
        }
    }
}

#[test]
fn water_and_out_of_bounds_end_the_shot() {
    let wet = hit(0, 1.0, &Shore);
    assert_eq!(wet.end(), Event::Splash, "a drive lands in the water");
    let last = wet.samples.last().unwrap();
    assert!(last.position.x >= 150.0);
    // A shot that clears the water finds the out of bounds beyond it.
    let mut launch = BAG[0].launch(Vec3::new(0.0, 0.02, 0.0), EAST, 1.0, 0.0);
    launch.speed *= 1.4;
    let far = play_out(&launch, Wind::default(), &Shore);
    assert!(matches!(far.end(), Event::OutOfBounds | Event::Splash));
    // A ball rolling into the water is a splash too.
    let creeping = hit(PUTTER_INDEX, 0.75, &Shore);
    assert!(creeping.end().is_final());
}

/// A wall of tree at x = 100, taller than any drive.
struct Wood(Plane);

impl Terrain for Wood {
    fn height(&self, x: f32, z: f32) -> f32 {
        self.0.height(x, z)
    }
    fn material(&self, x: f32, z: f32) -> Material {
        self.0.material(x, z)
    }
    fn solid(&self, p: Vec3) -> bool {
        (100.0..101.0).contains(&p.x) && p.y < 60.0
    }
}

#[test]
fn a_tree_stops_the_ball_short() {
    let f = hit(0, 1.0, &Wood(plane(Material::Fairway)));
    assert_eq!(count(&f, Event::TreeHit), 1);
    let end = f.samples.last().unwrap().position;
    assert!(end.x < 105.0, "dropped at the tree: {}", end.x);
    assert_eq!(f.end(), Event::Rest);
}

#[test]
fn every_shot_ends() {
    // A seeded spread of shots over a hill: a final event, finite samples,
    // positions that stay finite.
    let hill = Plane {
        slope: Vec2::new(0.05, -0.03),
        material: Material::Fairway,
        pin: Some(Vec2::new(60.0, 4.0)),
    };
    let mut seed = 0x2545_f491u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f32 / 16_777_216.0
    };
    for _ in 0..150 {
        let club = (next() * BAG.len() as f32) as usize % BAG.len();
        let power = next() * 1.2;
        let heading = Vec2::from_angle(next() * std::f32::consts::TAU);
        let side = (next() - 0.5) * 4000.0;
        let at = Vec3::new(0.0, hill.height(0.0, 0.0) + 0.02, 0.0);
        let launch = BAG[club].launch(at, heading, power, side);
        let f = play_out(&launch, Wind::blowing(Vec2::new(0.0, 1.0), 5.0), &hill);
        assert!(f.end().is_final(), "{club} {power}: ended {:?}", f.end());
        assert!(f.samples.len() < 40_000);
        assert!(f.samples.iter().all(|s| s.position.length().is_finite()));
        assert!(f.samples.windows(2).all(|w| w[1].t >= w[0].t));
    }
}

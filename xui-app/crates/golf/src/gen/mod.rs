//! The course generator (docs/golf-course-generator.md, Part I):
//!
//! ```text
//! seed -> 1. site terrain -> 2. hydrology -> 3. routing -> 4. hole layout
//!      -> 5. sculpting -> 6. materials -> 7. objects and trees -> 8. par
//! ```
//!
//! A seed names a *search*: many candidate sites are surveyed from cheap
//! terrain previews, the most promising are built and routed a few times,
//! and only the best routing (`quality`) is laid out and finished. Every
//! stage draws from its own RNG derived from its candidate's seed.
//! [`Generator`] runs one step at a time, so a window can show progress;
//! [`generate`] runs them all.

mod cartpath;
mod features;
mod hydro;
mod landmarks;
mod layout;
mod objects;
mod paint;
mod par;
pub mod quality;
pub mod routing;
mod sculpt;
mod search;
mod shape;
mod terrain;
mod trees;

use crate::course::{Archetype, Course, Hole, Material, Object, ObjectKind, TeeSet};
use crate::field::Field;
use crate::math::Vec2;
use crate::rng::Rng;

pub use par::{putts, BOGEY, SCRATCH};
pub use quality::Quality;
pub use trees::crown;

use search::{Routed, SiteWork};

/// What to generate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub seed: u64,
    /// `None` lets each candidate site pick one from its seed.
    pub archetype: Option<Archetype>,
    /// Cells (metres) on a side.
    pub size: usize,
    /// Sites previewed.
    pub survey: usize,
    /// Of those, how many are built and routed.
    pub finalists: usize,
    /// Routings tried on each finalist.
    pub routings: usize,
}

impl Params {
    /// The full search: 16 sites surveyed, 3 built, 2 routings each.
    pub fn new(seed: u64) -> Params {
        Params {
            seed,
            archetype: None,
            size: 1024,
            survey: 16,
            finalists: 3,
            routings: 2,
        }
    }

    /// No search: the seed's own site and first routing.
    pub fn quick(seed: u64) -> Params {
        Params {
            survey: 1,
            finalists: 1,
            routings: 1,
            ..Params::new(seed)
        }
    }

    pub fn with_archetype(self, archetype: Archetype) -> Params {
        Params {
            archetype: Some(archetype),
            ..self
        }
    }
}

/// The step a generator runs next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Survey,
    Terrain,
    Hydrology,
    Routing,
    Layout,
    Sculpting,
    Painting,
    Objects,
    Par,
    Done,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Survey => "Surveying sites",
            Stage::Terrain => "Raising the land",
            Stage::Hydrology => "Letting the rain run off",
            Stage::Routing => "Routing 18 holes",
            Stage::Layout => "Laying out fairways and greens",
            Stage::Sculpting => "Shaping tees, greens and bunkers",
            Stage::Painting => "Mowing and painting",
            Stage::Objects => "Planting trees",
            Stage::Par => "Playing every hole to find par",
            Stage::Done => "Ready",
        }
    }
}

/// The best candidate so far: its site and routing.
struct Best {
    site: SiteWork,
    routed: Routed,
}

/// A course being generated, one step at a time.
pub struct Generator {
    params: Params,
    stage: Stage,
    finalists: Vec<(u64, Archetype)>,
    /// The finalist being built and its routing try.
    current: usize,
    attempt: usize,
    site: Option<SiteWork>,
    /// The current site's best routing.
    site_best: Option<Routed>,
    best: Option<Best>,
    steps_done: usize,
    // The winner, once chosen.
    seed: u64,
    archetype: Archetype,
    elevation: Field<f32>,
    water: Field<bool>,
    water_level: Field<f32>,
    moisture: Field<f32>,
    landmarks: landmarks::Landmarks,
    clubhouse: Vec2,
    routes: Vec<routing::Route>,
    quality: Quality,
    plans: Vec<layout::HolePlan>,
    features: Option<features::Features>,
    material: Field<Material>,
    objects: Vec<Object>,
    pins: Vec<Vec2>,
    relaxed: Option<Option<usize>>,
}

impl Generator {
    pub fn new(params: Params) -> Generator {
        let empty = Field::new(0, 0, 0.0f32);
        Generator {
            params,
            stage: Stage::Survey,
            finalists: Vec::new(),
            current: 0,
            attempt: 0,
            site: None,
            site_best: None,
            best: None,
            steps_done: 0,
            seed: params.seed,
            archetype: Archetype::Parkland,
            elevation: empty.clone(),
            water: Field::new(0, 0, false),
            water_level: empty.clone(),
            moisture: empty,
            landmarks: landmarks::Landmarks::none(0),
            clubhouse: Vec2::default(),
            routes: Vec::new(),
            quality: Quality::default(),
            plans: Vec::new(),
            features: None,
            material: Field::new(0, 0, Material::Rough),
            objects: Vec::new(),
            pins: Vec::new(),
            relaxed: None,
        }
    }

    /// The step the next [`Generator::step`] runs.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// What is happening, for a progress display, and how far along, 0..1.
    pub fn progress(&self) -> (String, f32) {
        let p = &self.params;
        let total = 1 + p.finalists.max(1) * (2 + p.routings.max(1)) + 5;
        let label = match self.stage {
            Stage::Survey => format!("Surveying {} sites", p.survey.max(1)),
            Stage::Terrain | Stage::Hydrology | Stage::Routing if self.finalists.len() > 1 => {
                format!(
                    "{} (site {} of {})",
                    self.stage.label(),
                    self.current + 1,
                    self.finalists.len()
                )
            }
            stage => stage.label().to_owned(),
        };
        (label, self.steps_done as f32 / total as f32)
    }

    /// After routing: how many times the chosen routing loosened its
    /// constraints to fit 18 holes (`Some(None)`: its fallback grid).
    pub fn routing_relaxation(&self) -> Option<Option<usize>> {
        self.relaxed
    }

    /// The chosen routing's score, once chosen.
    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// Runs one step; returns the course after the last one.
    pub fn step(&mut self) -> Option<Course> {
        let size = self.params.size;
        self.steps_done += 1;
        match self.stage {
            Stage::Survey => {
                self.finalists = search::survey(&self.params);
                self.stage = Stage::Terrain;
            }
            Stage::Terrain => {
                let (seed, archetype) = self.finalists[self.current];
                let empty = Field::new(0, 0, 0.0f32);
                let mut elevation = terrain::site(seed, archetype, size);
                let landmarks = landmarks::place(&mut elevation, archetype, seed);
                self.site = Some(SiteWork {
                    seed,
                    archetype,
                    elevation,
                    water: Field::new(0, 0, false),
                    water_level: empty.clone(),
                    moisture: empty,
                    landmarks,
                });
                self.stage = Stage::Hydrology;
            }
            Stage::Hydrology => {
                let site = self.site.as_mut().expect("raised");
                let h = hydro::run(&mut site.elevation);
                site.water = h.water;
                site.water_level = h.water_level;
                site.moisture = h.moisture;
                self.attempt = 0;
                self.stage = Stage::Routing;
            }
            Stage::Routing => self.route_attempt(),
            Stage::Layout => {
                self.plans = layout::lay_out(&self.routes, self.seed);
                self.stage = Stage::Sculpting;
            }
            Stage::Sculpting => {
                let features = features::rasterize(&self.plans, size);
                sculpt::sculpt(
                    &mut self.elevation,
                    &mut self.water,
                    &self.water_level,
                    &features,
                    &self.plans,
                    self.clubhouse,
                    self.seed,
                );
                self.features = Some(features);
                self.stage = Stage::Painting;
            }
            Stage::Painting => {
                self.paint();
                self.stage = Stage::Objects;
            }
            Stage::Objects => {
                self.place_objects();
                self.stage = Stage::Par;
            }
            Stage::Par => {
                self.stage = Stage::Done;
                return Some(self.finish());
            }
            Stage::Done => {}
        }
        None
    }

    /// One routing try on the current site; after the site's last try it
    /// competes with the best so far, and after the last site the winner
    /// moves in.
    fn route_attempt(&mut self) {
        let site = self.site.as_ref().expect("built");
        let routed = search::route(site, self.params.size, self.attempt);
        if self
            .site_best
            .as_ref()
            .is_none_or(|b| routed.quality.total > b.quality.total)
        {
            self.site_best = Some(routed);
        }
        self.attempt += 1;
        if self.attempt < self.params.routings.max(1) {
            return;
        }
        let site = self.site.take().expect("built");
        let routed = self.site_best.take().expect("routed");
        if self
            .best
            .as_ref()
            .is_none_or(|b| routed.quality.total > b.routed.quality.total)
        {
            self.best = Some(Best { site, routed });
        }
        self.current += 1;
        if self.current < self.finalists.len() {
            self.stage = Stage::Terrain;
            return;
        }
        let Best { site, routed } = self.best.take().expect("one finalist at least");
        self.seed = site.seed;
        self.archetype = site.archetype;
        self.elevation = site.elevation;
        self.water = site.water;
        self.water_level = site.water_level;
        self.moisture = site.moisture;
        self.landmarks = site.landmarks;
        self.clubhouse = routed.clubhouse;
        self.routes = routed.routes;
        self.relaxed = Some(routed.relaxed);
        self.quality = routed.quality;
        self.stage = Stage::Layout;
    }

    fn paint_input(&self) -> paint::PaintInput<'_> {
        paint::PaintInput {
            elevation: &self.elevation,
            water: &self.water,
            moisture: &self.moisture,
            features: self.features.as_ref().expect("sculpted"),
            archetype: self.archetype,
            seed: self.seed,
            landmarks: &self.landmarks,
        }
    }

    fn paint(&mut self) {
        let mut material = paint::paint(&self.paint_input());
        let mut rng = Rng::stage(self.seed, "clubhouse");
        objects::clubhouse_grounds(&mut material, self.clubhouse, &mut rng);
        let paths = cartpath::plan(&material, &self.elevation, &self.plans, self.clubhouse);
        let bridges = cartpath::lay(&mut material, &paths);
        self.objects = objects::fixed(
            &self.plans,
            &self.elevation,
            self.clubhouse,
            &bridges,
            self.params.size,
        );
        self.material = material;
    }

    fn place_objects(&mut self) {
        let mut rng = Rng::stage(self.seed, "trees");
        let trees = trees::place(&self.paint_input(), &self.material, &self.plans, &mut rng);
        self.objects.extend(trees);
        let mut rng = Rng::stage(self.seed, "pins");
        self.pins = self
            .plans
            .iter()
            .map(|p| objects::pin(p, &self.elevation, &mut rng))
            .collect();
        for (i, &pin) in self.pins.iter().enumerate() {
            self.objects.push(Object {
                kind: ObjectKind::Flagstick { hole: i as u8 + 1 },
                position: pin,
                base: self.elevation.sample(pin.x, pin.y),
                height: 2.3,
            });
        }
    }

    /// Stage 8 and the hand-over: par, ratings, the finished course.
    fn finish(&mut self) -> Course {
        let mut holes: Vec<Hole> = Vec::with_capacity(self.plans.len());
        for (i, plan) in self.plans.iter().enumerate() {
            let pin = self.pins[i];
            let (scratch, reach) = par::expected(plan, pin, &self.material, &par::SCRATCH);
            let (bogey, _) = par::expected(plan, pin, &self.material, &par::BOGEY);
            let effective = par::effective_length(plan, &self.elevation, &self.material);
            holes.push(Hole {
                number: i as u8 + 1,
                par: par::par(reach, effective),
                tees: plan
                    .tees
                    .iter()
                    .map(|t| TeeSet {
                        name: t.name,
                        position: t.pad.center,
                        length_m: plan.length - t.along,
                    })
                    .collect(),
                centerline: plan.centerline.clone(),
                green: plan.green.polygon(32),
                green_center: plan.green.center,
                pin,
                bunkers: plan.bunkers.iter().map(|b| b.polygon(20)).collect(),
                landing_zones: plan.landing,
                length_m: plan.length,
                effective_length_m: effective,
                scratch_expected: scratch,
                bogey_expected: bogey,
                handicap_index: 0,
            });
        }
        stroke_indexes(&mut holes);
        let rating: f32 = holes.iter().map(|h| h.scratch_expected).sum();
        let bogey: f32 = holes.iter().map(|h| h.bogey_expected).sum();
        let slope = (5.381 * (bogey - rating)).round().clamp(55.0, 155.0) as u16;
        let features = self.features.take().expect("sculpted");
        let material = std::mem::replace(&mut self.material, Field::new(0, 0, Material::Rough));
        Course {
            seed: self.params.seed,
            site_seed: self.seed,
            quality: self.quality.total,
            archetype: self.archetype,
            name: course_name(self.seed, self.archetype),
            cell_size_m: 1.0,
            width: self.params.size as u32,
            height: self.params.size as u32,
            edge_sdf: paint::edge_distance(&material),
            elevation: std::mem::replace(&mut self.elevation, Field::new(0, 0, 0.0)),
            material,
            moisture: std::mem::replace(&mut self.moisture, Field::new(0, 0, 0.0)),
            hole_of: features.hole_of,
            water_level: std::mem::replace(&mut self.water_level, Field::new(0, 0, 0.0)),
            objects: std::mem::take(&mut self.objects),
            par: holes.iter().map(|h| h.par).sum(),
            holes,
            clubhouse: self.clubhouse,
            rating: (rating * 10.0).round() / 10.0,
            slope,
        }
    }
}

/// Stroke indexes: the hardest hole relative to par (bogey model) is 1;
/// odd indexes go to the front nine and even ones to the back, as is usual.
fn stroke_indexes(holes: &mut [Hole]) {
    for (nine, first) in [
        (0..9.min(holes.len()), 1u8),
        (9.min(holes.len())..holes.len(), 2u8),
    ] {
        let mut order: Vec<usize> = nine.collect();
        order.sort_by(|&a, &b| {
            let d = |h: &Hole| h.bogey_expected - f32::from(h.par);
            d(&holes[b]).total_cmp(&d(&holes[a]))
        });
        for (rank, &i) in order.iter().enumerate() {
            holes[i].handicap_index = first + 2 * rank as u8;
        }
    }
}

/// A name for the course from its seed.
fn course_name(seed: u64, archetype: Archetype) -> String {
    const FIRST: [&str; 12] = [
        "Lazy",
        "Heron",
        "Oak",
        "Kestrel",
        "Saltmarsh",
        "Whispering",
        "Granite",
        "Fox",
        "Willow",
        "Pebble",
        "Crow",
        "Larch",
    ];
    const SECOND: [&str; 8] = [
        "Hollow", "Ridge", "Creek", "Downs", "Point", "Valley", "Heath", "Bay",
    ];
    let mut rng = Rng::stage(seed, "name");
    let suffix = match archetype {
        Archetype::Links => "Links",
        Archetype::Parkland => "Golf Club",
        Archetype::Mountain => "Mountain Course",
    };
    format!(
        "{} {} {}",
        FIRST[rng.below(FIRST.len())],
        SECOND[rng.below(SECOND.len())],
        suffix
    )
}

/// Generates the whole course.
pub fn generate(params: Params) -> Course {
    let mut generator = Generator::new(params);
    loop {
        if let Some(course) = generator.step() {
            return course;
        }
    }
}

//! The search for a good course: survey many sites from cheap terrain
//! previews, build and route only the most promising, and keep the routing
//! that scores best (see `quality`). The search is seeded, so a seed still
//! names one course.

use crate::course::Archetype;
use crate::field::Field;
use crate::math::Vec2;
use crate::rng::{mix, Rng};

use super::landmarks::Landmarks;
use super::quality::{self, Quality};
use super::routing::{self, Route};
use super::{terrain, Params};

/// Preview resolution: one sample every this many metres.
const PREVIEW_STRIDE: usize = 4;

/// The seed of the search's `i`th candidate site; the first is the course
/// seed itself, so a search of one is the plain seed.
pub fn candidate_seed(seed: u64, i: usize) -> u64 {
    if i == 0 {
        seed
    } else {
        mix(seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }
}

/// The landform every candidate shares: the requested one, or one picked by
/// the course seed (so seeds still vary between links, parkland and
/// mountain; the search finds the best site of that kind).
pub fn archetype(params: &Params) -> Archetype {
    params
        .archetype
        .unwrap_or(Archetype::ALL[Rng::stage(params.seed, "archetype").below(Archetype::ALL.len())])
}

/// The `params.finalists` most promising of `params.survey` sites, best
/// first, with the seed's own site among them.
pub fn survey(params: &Params) -> Vec<(u64, Archetype)> {
    let mut scored: Vec<(f32, u64, Archetype)> = (0..params.survey.max(1))
        .map(|i| {
            let seed = candidate_seed(params.seed, i);
            let archetype = archetype(params);
            let preview = terrain::preview(seed, archetype, params.size, PREVIEW_STRIDE);
            let score = quality::site(&preview, PREVIEW_STRIDE as f32, archetype.relief_scale());
            (score, seed, archetype)
        })
        .collect();
    // The seed's own site always competes, so the search never ends up
    // worse than the plain seed would have been.
    let own = scored[0];
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut finalists: Vec<(u64, Archetype)> = scored
        .into_iter()
        .take(params.finalists.max(1))
        .map(|(_, seed, archetype)| (seed, archetype))
        .collect();
    if !finalists.iter().any(|f| f.0 == own.1) {
        *finalists.last_mut().expect("at least one finalist") = (own.1, own.2);
    }
    finalists
}

/// A site being built: its land and water.
pub struct SiteWork {
    pub seed: u64,
    pub archetype: Archetype,
    pub elevation: Field<f32>,
    pub water: Field<bool>,
    pub water_level: Field<f32>,
    pub moisture: Field<f32>,
    pub landmarks: Landmarks,
}

/// A routed site and how good it is.
pub struct Routed {
    pub clubhouse: Vec2,
    pub routes: Vec<Route>,
    pub relaxed: Option<usize>,
    pub quality: Quality,
}

/// Routes `site` (try `attempt`, each with its own draws) and scores it.
pub fn route(site: &SiteWork, size: usize, attempt: usize) -> Routed {
    let mut s = routing::Site {
        elevation: &site.elevation,
        water: &site.water,
        size: size as f32,
        clubhouse: Vec2::default(),
        buffer: 62.0,
        max_slope: 0.1,
        relief: site.archetype.relief_scale(),
        blocked: &site.landmarks.blocked,
        front: Vec2::default(),
    };
    let mut rng = Rng::stage(site.seed ^ attempt as u64, "routing");
    s.clubhouse = routing::clubhouse(&s, &mut rng);
    s.front = routing::split(&s, &mut rng);
    let pars = routing::par_mix(&mut rng);
    let clubhouse = s.clubhouse;
    let (routes, relaxed) = routing::route(&mut s, &pars, &mut rng);
    let quality = quality::routing(&s, &routes, relaxed);
    Routed {
        clubhouse,
        routes,
        relaxed,
        quality,
    }
}

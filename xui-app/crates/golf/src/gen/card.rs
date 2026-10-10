//! The course card: its name and each hole's stroke index.

use crate::course::{Archetype, Hole};
use crate::rng::Rng;

/// Stroke indexes: the hardest hole relative to par (bogey model) is 1;
/// odd indexes go to the front nine and even ones to the back, as is usual.
pub fn stroke_indexes(holes: &mut [Hole]) {
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
pub fn course_name(seed: u64, archetype: Archetype) -> String {
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

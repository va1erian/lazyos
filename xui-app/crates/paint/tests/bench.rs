//! The stroke-to-frame cost on a big picture. Ignored by default (it holds
//! about 400 MiB and wants a release build):
//!
//! ```text
//! cargo test -p xui-paint --release --test bench -- --ignored --nocapture
//! ```
//!
//! One step is what a mouse-move costs: the model draws the segment, the
//! flattened cache recomputes the dirty rectangle over four layers, and the
//! screen image of a 1024 x 768 window is rebuilt. Painting it is `draw_image`
//! of that image 1:1 and is not measured here.

use std::time::Instant;

use xui_paint::model::{Model, Side, Tool};
use xui_paint::render::{FlatCache, render};
use xui_paint::viewport::Viewport;

const VIEW: (i32, i32) = (1024, 768);
/// The target p99 for one stroke-to-frame step.
const BUDGET_MS: f64 = 8.0;

fn p99(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[(samples.len() * 99 / 100).min(samples.len() - 1)]
}

fn run(zoom: f64) -> (f64, f64) {
    let mut model = Model::try_new(4096, 4096).expect("4096 x 4096 fits");
    for _ in 0..3 {
        model.add_layer().expect("four layers fit");
    }
    model.set_tool(Tool::Brush);
    model.set_size(12);
    model.set_primary([200, 30, 30, 255]);
    let mut viewport = Viewport::new();
    viewport.zoom_to(zoom, (0, 0), (4096, 4096), VIEW);
    let mut cache = FlatCache::new();
    cache.update(model.document(), model.take_damage());

    let mut steps = Vec::new();
    model.begin(100, 100, Side::Primary);
    for i in 0..600 {
        let t = Instant::now();
        let x = 100 + i * 3;
        let y = 100 + ((i as f64 / 20.0).sin() * 60.0) as i32 + i / 2;
        model.extend(x, y);
        cache.update(model.document(), model.take_damage());
        let image = render(cache.get().unwrap(), &viewport, VIEW, true).expect("visible");
        assert_eq!((image.width, image.height), (1024, 768));
        steps.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    model.end();
    let mean = steps.iter().sum::<f64>() / steps.len() as f64;
    (mean, p99(steps))
}

#[test]
#[ignore = "heavy; run with --release -- --ignored"]
fn a_stroke_on_a_4096_square_four_layer_image_stays_in_budget() {
    for zoom in [1.0, 0.25, 8.0] {
        let (mean, p99) = run(zoom);
        println!("zoom {zoom}: mean {mean:.2} ms, p99 {p99:.2} ms (budget {BUDGET_MS} ms)");
        if !cfg!(debug_assertions) {
            assert!(p99 <= BUDGET_MS, "zoom {zoom}: p99 {p99:.2} ms");
        }
    }
}

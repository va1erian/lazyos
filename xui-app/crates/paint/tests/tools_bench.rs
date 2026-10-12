//! A big soft brush on a big picture. Ignored by default (it holds about
//! 64 MiB and wants a release build):
//!
//! ```text
//! cargo test -p xui-paint --release --test tools_bench -- --ignored --nocapture
//! ```
//!
//! One step is what a mouse-move costs: the 200 px soft brush segment into
//! the stroke buffer, and the composite of what it added onto a 4096 x 4096
//! layer.

use std::time::Instant;

use xui_paint::model::Bitmap;
use xui_paint::model::tools::{
    BrushOptions, Gradient, GradientKind, GradientOptions, PaintMode, StrokeBuffer,
};

/// The target for one segment, in milliseconds.
const BUDGET_MS: f64 = 2.0;

fn p99(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[(samples.len() * 99 / 100).min(samples.len() - 1)]
}

#[test]
#[ignore = "heavy; run with --release -- --ignored"]
fn a_200px_soft_brush_segment_stays_in_budget_on_a_4096_image() {
    let mut layer = Bitmap::white(4096, 4096);
    let brush = BrushOptions {
        width: 200,
        hardness: 20,
        antialias: true,
    };
    let mut stroke = StrokeBuffer::new(&layer);
    let (mut brush_ms, mut total_ms) = (Vec::new(), Vec::new());
    // A mouse moving 8 px per event along a wave across the picture.
    let at = |t: f32| (150.0 + t * 3.7, 600.0 + t * 0.6 + (t * 0.05).sin() * 120.0);
    for i in 0..1000 {
        let (a, b) = (at(i as f32), at(i as f32 + 1.0));
        let start = Instant::now();
        stroke.segment(&layer, a, b, brush);
        let drawn = start.elapsed();
        stroke.composite(&mut layer, [30, 90, 200, 220], PaintMode::Paint, None);
        brush_ms.push(drawn.as_secs_f64() * 1000.0);
        total_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = total_ms.iter().sum::<f64>() / total_ms.len() as f64;
    let (brush_p99, total_p99) = (p99(brush_ms), p99(total_ms));
    println!(
        "segment + composite: mean {mean:.3} ms, p99 {total_p99:.3} ms (brush alone p99 {brush_p99:.3} ms; budget {BUDGET_MS} ms)"
    );
    if !cfg!(debug_assertions) {
        assert!(total_p99 <= BUDGET_MS, "p99 {total_p99:.3} ms");
    }
}

/// One gradient drag move on a 4096 x 4096 layer through the model: the last
/// frame is taken back from the undo capture and the whole layer is painted
/// again.
#[test]
#[ignore = "heavy; run with --release -- --ignored"]
fn a_gradient_drag_move_on_a_4096_image_is_fast() {
    use xui_paint::model::{Model, Side, Tool};
    for kind in [
        GradientKind::Linear,
        GradientKind::LinearReflected,
        GradientKind::Radial,
        GradientKind::Diamond,
    ] {
        let mut model = Model::new(4096, 4096);
        model.set_gradient(GradientOptions { kind });
        model.set_primary([255, 0, 0, 255]);
        model.set_secondary([0, 0, 255, 160]);
        model.set_tool(Tool::Gradient);
        model.begin(100, 100, Side::Primary);
        let mut moves = Vec::new();
        for i in 0..20 {
            let start = Instant::now();
            model.extend(1000 + i * 90, 3000 - i * 70);
            moves.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        model.end();
        // The paint alone, without the capture's restore.
        let g = Gradient::new(
            GradientOptions { kind },
            (100.5, 100.5),
            (2000.5, 1500.5),
            [255, 0, 0, 255],
            [0, 0, 255, 160],
        );
        let mut layer = Bitmap::white(4096, 4096);
        let full = layer.rect();
        let start = Instant::now();
        g.paint(&mut layer, full, None);
        println!(
            "gradient {kind:?}: paint alone {:.1} ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
        let mean = moves.iter().sum::<f64>() / moves.len() as f64;
        println!(
            "gradient {kind:?} 4096 x 4096: mean {mean:.1} ms, p99 {:.1} ms per move",
            p99(moves)
        );
    }
}

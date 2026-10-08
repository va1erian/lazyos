//! The Overview tab's painted pieces, which xui has no widget for: the CPU
//! history graph, the memory bar split into coloured shares, and the colour
//! swatch beside each share's name in the legend.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::theme::look::backdrop;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Dip, Theme};

use crate::Msg;

/// One share of memory, in the order the bar paints them left to right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Share {
    Programs,
    System,
    Cache,
    Free,
}

impl Share {
    pub(crate) const ALL: [Share; 4] = [Share::Programs, Share::System, Share::Cache, Share::Free];

    /// The share's colour; free memory is the bar's empty track.
    fn color(self, theme: &Theme) -> Color {
        let dark = theme.is_dark;
        match self {
            Share::Programs if dark => Color::hex(0x60A5FA),
            Share::Programs => Color::hex(0x2563EB),
            Share::System if dark => Color::hex(0xC084FC),
            Share::System => Color::hex(0x9333EA),
            Share::Cache if dark => Color::hex(0xFBBF24),
            Share::Cache => Color::hex(0xD97706),
            Share::Free => track(theme),
        }
    }
}

/// The empty part of a bar or graph.
fn track(theme: &Theme) -> Color {
    theme.input_background
}

/// A node painted by `paint` from a shared value, sized by the layout.
pub(crate) struct Painted<T> {
    control: Control<Msg>,
    value: Rc<RefCell<T>>,
    /// The natural size in design pixels (`0` lets the layout decide).
    natural: (f32, f32),
}

impl<T: Default + 'static> Painted<T> {
    fn new(
        ui: &Ui<Msg>,
        natural: (f32, f32),
        paint: impl Fn(&mut dyn Canvas, &T, &Theme) + 'static,
    ) -> Result<Painted<T>> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let value = Rc::new(RefCell::new(T::default()));
        let theme = ui.theme_handle();
        let shown = Rc::clone(&value);
        control.set_painter(Rc::new(move |canvas| {
            let theme = theme.get();
            backdrop(canvas, theme.background);
            paint(canvas, &shown.borrow(), &theme);
        }));
        Ok(Painted {
            control,
            value,
            natural,
        })
    }

    /// Shows `value`.
    pub(crate) fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
        self.control.invalidate();
    }
}

impl<T: 'static> Placeable<Msg> for Painted<T> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let px = |dip: f32| Dip(dip).to_px(constraints.dpi).value();
        Size::new(px(self.natural.0), px(self.natural.1))
    }
}

/// The CPU graph: the last samples, in percent, oldest first.
pub(crate) type CpuGraph = Painted<Vec<u32>>;
/// The memory bar: each share's bytes, in [`Share::ALL`] order.
pub(crate) type MemoryBar = Painted<[u64; 4]>;
/// A legend swatch: which share it stands for.
pub(crate) type Swatch = Painted<Option<Share>>;

/// How many samples the graph spans (one a second).
pub(crate) const HISTORY: usize = 60;

pub(crate) fn cpu_graph(ui: &Ui<Msg>) -> Result<CpuGraph> {
    Painted::new(ui, (0.0, 0.0), |canvas, samples: &Vec<u32>, theme| {
        paint_graph(canvas, samples, theme)
    })
}

pub(crate) fn memory_bar(ui: &Ui<Msg>) -> Result<MemoryBar> {
    Painted::new(ui, (0.0, 22.0), paint_bar)
}

pub(crate) fn swatch(ui: &Ui<Msg>, share: Share) -> Result<Swatch> {
    let swatch = Painted::new(ui, (14.0, 14.0), paint_swatch)?;
    swatch.set(Some(share));
    Ok(swatch)
}

/// An area graph of CPU load: a filled polygon under a line, over grid lines
/// at every quarter, scrolling in from the right.
fn paint_graph(canvas: &mut dyn Canvas, samples: &[u32], theme: &Theme) {
    let area = canvas.bounds().shrink(1);
    canvas.fill_rounded_rect(area, 4.0, track(theme));
    let (width, height) = (area.width().max(1), area.height().max(1));
    for quarter in 1..4 {
        let y = area.top + height * quarter / 4;
        let line = theme.border.lerp(track(theme), 0.5);
        canvas.draw_line(
            Point::new(area.left, y),
            Point::new(area.right, y),
            line,
            1.0,
        );
    }
    let line = Share::Programs.color(theme);
    let step = width as f32 / (HISTORY - 1) as f32;
    let start = HISTORY.saturating_sub(samples.len());
    let points: Vec<Point> = samples
        .iter()
        .rev()
        .take(HISTORY)
        .rev()
        .enumerate()
        .map(|(index, percent)| {
            let x = area.left + ((start + index) as f32 * step).round() as i32;
            let y = area.bottom - (height as f32 * (*percent).min(100) as f32 / 100.0) as i32;
            Point::new(x, y)
        })
        .collect();
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        let mut polygon = points.clone();
        polygon.push(Point::new(last.x, area.bottom));
        polygon.push(Point::new(first.x, area.bottom));
        canvas.fill_polygon(&polygon, line.lerp(track(theme), 0.65));
        for pair in points.windows(2) {
            canvas.draw_line(pair[0], pair[1], line, 2.0);
        }
    }
    canvas.stroke_rounded_rect(area, 4.0, theme.border, 1.0);
}

/// One bar, its shares side by side in proportion; free memory is the track.
fn paint_bar(canvas: &mut dyn Canvas, shares: &[u64; 4], theme: &Theme) {
    let area = canvas.bounds().shrink(1);
    let radius = 4.0;
    canvas.fill_rounded_rect(area, radius, track(theme));
    let total: u64 = shares.iter().sum();
    if total > 0 {
        let mut left = area.left;
        let mut so_far = 0u64;
        for (share, bytes) in Share::ALL.iter().zip(shares).take(3) {
            so_far += bytes;
            let right = area.left + (area.width() as u128 * so_far as u128 / total as u128) as i32;
            if right > left {
                let piece = Rect::new(left, area.top, right, area.bottom);
                canvas.fill_rect(piece, share.color(theme));
            }
            left = right;
        }
    }
    canvas.stroke_rounded_rect(area, radius, theme.border, 1.0);
}

fn paint_swatch(canvas: &mut dyn Canvas, share: &Option<Share>, theme: &Theme) {
    let Some(share) = share else { return };
    let bounds = canvas.bounds();
    let side = bounds.width().min(bounds.height());
    let top = bounds.top + (bounds.height() - side) / 2;
    let square = Rect::new(bounds.left, top, bounds.left + side, top + side).shrink(1);
    canvas.fill_rounded_rect(square, 3.0, share.color(theme));
    // Free memory is the bar's empty track: outline it so it reads as a box.
    let outline = if share == &Share::Free {
        theme.text_secondary
    } else {
        theme.border
    };
    canvas.stroke_rounded_rect(square, 3.0, outline, 1.0);
}

/// Keeps the newest [`HISTORY`] samples.
#[derive(Default)]
pub(crate) struct History {
    samples: Vec<u32>,
}

impl History {
    pub(crate) fn push(&mut self, percent: u32) {
        if self.samples.len() == HISTORY {
            self.samples.remove(0);
        }
        self.samples.push(percent.min(100));
    }

    pub(crate) fn samples(&self) -> &[u32] {
        &self.samples
    }
}

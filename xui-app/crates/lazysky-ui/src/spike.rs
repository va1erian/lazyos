//! The P0 spike: text shaped on a worker thread, drawn on the UI thread
//! (docs/lazysky-plan.md §6.1, §9 P0). `TextShaper` and `TextLayout` are
//! `Send + Sync`; this proves `Canvas::draw_layout` accepts a layout the
//! shaper made elsewhere, under whichever backend hosts the window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use xui_core::app::Ui;
use xui_core::backend::{
    Canvas, FontSpec, NodeKind, NodeSpec, Result, Rgba, TextLayout, TextShaper, TimerId, WidgetId,
};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Dip, Theme};

use crate::{Msg, Report};

/// How often the view looks for the worker's answer.
const TICK_MS: u32 = 16;
/// The paragraph's wrap width at 96 dpi.
const WRAP_96: f32 = 420.0;

const TEXT: &str = "LazySky: the feed shapes every post on a layout thread, so scrolling \
never waits on text. This paragraph was shaped off the UI thread and is drawn here with \
Canvas::draw_layout.";

/// What the worker returns: a title and a wrapped paragraph.
struct Shaped {
    title: Box<dyn TextLayout>,
    body: Box<dyn TextLayout>,
}

/// Shapes the spike's text on a new thread; the layouts come back on the
/// channel.
fn shape_off_thread(shaper: Box<dyn TextShaper>, dpi: u32) -> Receiver<Shaped> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("sky-layout".into())
        .spawn(move || {
            let scale = dpi.max(1) as f32 / 96.0;
            let title = shaper.layout(
                "LazySky",
                &FontSpec::new(Dip(24.0)).weight(700),
                f32::INFINITY,
                dpi,
            );
            let body = shaper.layout(TEXT, &FontSpec::new(Dip(14.0)), WRAP_96 * scale, dpi);
            let _ = tx.send(Shaped { title, body });
        })
        .expect("the layout thread starts");
    rx
}

pub struct SpikeView {
    control: Control<Msg>,
    ui: Ui<Msg>,
    shaped: Rc<RefCell<Option<Shaped>>>,
    answers: Receiver<Shaped>,
    timer: Cell<Option<TimerId>>,
}

impl SpikeView {
    pub fn new(ui: &Ui<Msg>) -> Result<SpikeView> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let shaped: Rc<RefCell<Option<Shaped>>> = Rc::default();
        {
            let shaped = Rc::clone(&shaped);
            let theme = ui.theme_handle();
            let dpi = ui.dpi();
            control.set_painter(Rc::new(move |canvas| {
                paint(canvas, &shaped.borrow(), &theme.get(), dpi)
            }));
        }
        let answers = shape_off_thread(ui.text_shaper(), ui.dpi());
        let timer = control.set_timer(TICK_MS, || Some(Msg::Poll));
        Ok(SpikeView {
            control,
            ui: ui.clone(),
            shaped,
            answers,
            timer: Cell::new(timer),
        })
    }

    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Whether the worker's layouts have arrived.
    pub fn is_shaped(&self) -> bool {
        self.shaped.borrow().is_some()
    }

    /// A timer tick: takes the worker's answer when it is there, stops the
    /// timer, and says so once.
    pub fn poll(&self) -> Option<Report> {
        if self.is_shaped() {
            return None;
        }
        let answer = self.answers.try_recv().ok()?;
        let height = answer.body.height().ceil() as i32;
        *self.shaped.borrow_mut() = Some(answer);
        if let Some(timer) = self.timer.take() {
            self.control.kill_timer(timer);
        }
        self.ui.invalidate(self.control.id());
        Some(Report::ShapedOffThread {
            lines_height: height,
        })
    }
}

impl Placeable<Msg> for SpikeView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, _rect: Rect) {
        self.control.invalidate();
    }
}

fn paint(canvas: &mut dyn Canvas, shaped: &Option<Shaped>, theme: &Theme, dpi: u32) {
    let bounds = canvas.bounds();
    canvas.push_clip(bounds);
    canvas.clear(theme.background);
    if let Some(shaped) = shaped {
        let pad = (24 * dpi.max(1) as i32) / 96;
        let title_at = Point::new(bounds.left + pad, bounds.top + pad);
        canvas.draw_layout(&*shaped.title, title_at, Rgba::from(theme.accent));
        let body_at = Point::new(
            bounds.left + pad,
            title_at.y + shaped.title.height().ceil() as i32 + pad / 2,
        );
        canvas.draw_layout(&*shaped.body, body_at, Rgba::from(theme.text));
    }
    canvas.pop_clip();
}

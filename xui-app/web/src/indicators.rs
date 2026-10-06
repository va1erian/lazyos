//! Two small custom-painted widgets for the window's chrome: the throbber,
//! which spins while a page loads, and the security badge, a padlock in the
//! status bar that says whether the page came over HTTPS.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, TimerId, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::icon::{draw_icon, Lucide};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::Dip;

use crate::app::Msg;

/// The throbber's side and the badge's, in design units.
const THROBBER_SIZE: Dip = Dip(22.0);
const BADGE_SIZE: Dip = Dip(16.0);
/// Dots around the throbber, and how often it steps.
const DOTS: usize = 8;
const STEP_MS: u32 = 90;

/// The loading indicator: still and faint while idle, a turning ring of dots
/// while a page loads.
pub struct Throbber {
    control: Control<Msg>,
    /// The dot drawn brightest, which advances while spinning.
    phase: Rc<Cell<usize>>,
    spinning: Rc<Cell<bool>>,
    timer: Cell<Option<TimerId>>,
}

impl Throbber {
    pub fn new(ui: &Ui<Msg>) -> Result<Throbber> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let phase = Rc::new(Cell::new(0));
        let spinning = Rc::new(Cell::new(false));
        {
            let (phase, spinning) = (Rc::clone(&phase), Rc::clone(&spinning));
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                let theme = theme.get();
                let bounds = canvas.bounds();
                canvas.fill_rect(bounds, theme.background);
                let dpi = canvas.dpi();
                let side = THROBBER_SIZE.to_px(dpi).value() as f32;
                let center = Point::new(
                    (bounds.left + bounds.right) / 2,
                    (bounds.top + bounds.bottom) / 2,
                );
                let ring = side * 0.36;
                let dot = side * 0.09;
                for i in 0..DOTS {
                    let angle = i as f32 / DOTS as f32 * TAU;
                    let at = Point::new(
                        center.x + (angle.cos() * ring).round() as i32,
                        center.y + (angle.sin() * ring).round() as i32,
                    );
                    let color = if spinning.get() {
                        // The lead dot is the accent; the ones behind it fade.
                        let behind = (phase.get() + DOTS - i) % DOTS;
                        theme
                            .accent
                            .lerp(theme.background, behind as f32 / DOTS as f32)
                    } else {
                        theme.text_disabled
                    };
                    canvas.fill_ellipse(at, dot, dot, color);
                }
            }));
        }
        Ok(Throbber {
            control,
            phase,
            spinning,
            timer: Cell::new(None),
        })
    }

    /// Starts or stops the spinning.
    pub fn set_spinning(&self, on: bool) {
        if self.spinning.replace(on) == on {
            return;
        }
        if on {
            let (phase, control) = (Rc::clone(&self.phase), self.control.ui().clone());
            let id = self.control.id();
            self.timer.set(self.control.set_timer(STEP_MS, move || {
                phase.set((phase.get() + 1) % DOTS);
                control.invalidate(id);
                None
            }));
        } else if let Some(timer) = self.timer.take() {
            self.control.kill_timer(timer);
        }
        self.control.invalidate();
    }
}

impl Placeable<Msg> for Throbber {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        let side = THROBBER_SIZE.to_px(ui.dpi()).value();
        Size::new(side, side)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.control.set_bounds(rect);
    }
}

/// Whether a page came over a secure connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// Not a network page (the start page, a file): no badge.
    None,
    Secure,
    Insecure,
}

impl Security {
    /// The security of a page at `url`.
    pub fn of(url: &str) -> Security {
        let scheme = url.split_once(':').map_or("", |(s, _)| s);
        if scheme.eq_ignore_ascii_case("https") {
            Security::Secure
        } else if scheme.eq_ignore_ascii_case("http") {
            Security::Insecure
        } else {
            Security::None
        }
    }
}

/// The padlock in the status bar: closed and green over HTTPS, open and
/// amber over plain HTTP, nothing otherwise.
pub struct Badge {
    control: Control<Msg>,
    security: Rc<Cell<Security>>,
}

impl Badge {
    pub fn new(ui: &Ui<Msg>) -> Result<Badge> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let security = Rc::new(Cell::new(Security::None));
        {
            let security = Rc::clone(&security);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                let theme = theme.get();
                let bounds = canvas.bounds();
                canvas.fill_rect(bounds, theme.background);
                let (icon, color) = match security.get() {
                    Security::None => return,
                    Security::Secure => (Lucide::Lock, theme.accent),
                    Security::Insecure => (Lucide::Unlock, theme.warning),
                };
                let side = BADGE_SIZE.to_px(canvas.dpi()).value();
                let rect = Rect::new(
                    bounds.left,
                    (bounds.top + bounds.bottom - side) / 2,
                    bounds.left + side,
                    (bounds.top + bounds.bottom + side) / 2,
                );
                let dpi = canvas.dpi();
                draw_icon(canvas, icon, rect, color, dpi);
            }));
        }
        Ok(Badge { control, security })
    }

    pub fn set(&self, security: Security) {
        if self.security.replace(security) != security {
            self.control.invalidate();
        }
    }
}

impl Placeable<Msg> for Badge {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        let side = BADGE_SIZE.to_px(ui.dpi()).value();
        Size::new(side, side)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.control.set_bounds(rect);
    }
}

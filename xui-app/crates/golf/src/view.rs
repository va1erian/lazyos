//! The game widget: one custom-painted xui node that fills the window. A
//! repeating timer drives [`Game::tick`]; keys, drags and the wheel fly the
//! camera; the painter blits the frame and draws the HUD over it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::{Key, MouseButton};
use xui_core::widget::{Control, Placeable};

use crate::app::Msg;
use crate::fly::{action_for, EYE};
use crate::game::Game;
use crate::hud;
use crate::math::Vec3;

/// The timer period: ticks fast enough for 60 frames a second; a slow
/// frame simply delays the next tick.
const TICK_MS: u32 = 15;

/// A drag in progress: which button and where the pointer was.
#[derive(Clone, Copy)]
struct Drag {
    button: MouseButton,
    x: i32,
    y: i32,
}

pub struct GolfView {
    control: Control<Msg>,
    game: Rc<RefCell<Game>>,
}

impl GolfView {
    /// The widget, generating the course for `seed`.
    pub fn new(ui: &Ui<Msg>, seed: u64) -> Result<GolfView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let game = Rc::new(RefCell::new(Game::new(seed, ui.dpi())));
        {
            let game = Rc::clone(&game);
            // Never borrowed across a call back into xui (see `handle`), so
            // this only fails if a backend paints from inside a tick; the
            // next paint catches up.
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(game) = game.try_borrow() {
                    hud::paint(canvas, &game);
                }
            }));
        }
        {
            let (game, ui, id) = (Rc::clone(&game), ui.clone(), control.id());
            let drag = Rc::new(Cell::new(None));
            control.on_events(move |event| handle(&ui, id, &game, &drag, event));
        }
        {
            let (game, ui, id) = (Rc::clone(&game), ui.clone(), control.id());
            control.set_timer(TICK_MS, move || {
                let bounds = ui.bounds(id);
                let (w, h) = (
                    bounds.width().max(0) as usize,
                    bounds.height().max(0) as usize,
                );
                let (changed, report) = {
                    let mut game = game.try_borrow_mut().ok()?;
                    (game.tick(w, h, Instant::now()), game.take_report())
                };
                // The borrow ends first: a backend may paint (or deliver
                // events) from inside `invalidate`.
                if changed {
                    ui.invalidate(id);
                }
                report.map(Msg::Report)
            });
        }
        Ok(GolfView { control, game })
    }

    pub fn focus(&self) {
        self.control.focus();
    }

    /// The game, for a host or a test to drive and inspect.
    pub fn game(&self) -> Rc<RefCell<Game>> {
        Rc::clone(&self.game)
    }
}

impl Placeable<Msg> for GolfView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// What the event mapper asks of xui once it has let go of the game: the
/// winit backend delivers events (focus, capture) from inside these calls,
/// and they reach this mapper again.
#[derive(Default)]
struct After {
    focus: bool,
    capture: bool,
    release: bool,
    invalidate: bool,
}

/// The event mapper.
fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    game: &RefCell<Game>,
    drag: &Cell<Option<Drag>>,
    event: &Event,
) -> Option<Msg> {
    let bounds = ui.bounds(id);
    let scale = (ui.dpi() as i32 / 96).max(1);
    let mut after = After::default();
    let msg = {
        let Ok(mut game) = game.try_borrow_mut() else {
            return None;
        };
        apply(&mut game, drag, event, bounds, scale, &mut after)
    };
    if after.focus {
        ui.focus(id);
    }
    if after.capture {
        ui.set_capture(id);
    }
    if after.release {
        ui.release_capture();
    }
    if after.invalidate {
        ui.invalidate(id);
    }
    msg
}

/// One event's effect on the game; calls into xui are left in `after`.
fn apply(
    game: &mut Game,
    drag: &Cell<Option<Drag>>,
    event: &Event,
    bounds: Rect,
    scale: i32,
    after: &mut After,
) -> Option<Msg> {
    match *event {
        Event::KeyDown { key, repeat, .. } => {
            if let Some(action) = action_for(key) {
                game.held.press(action);
                return None;
            }
            if repeat > 1 {
                return None;
            }
            if key == Key::ESCAPE {
                return Some(Msg::Quit);
            }
            command(game, key);
            after.invalidate = true;
        }
        Event::KeyUp { key, .. } => {
            if let Some(action) = action_for(key) {
                game.held.release(action);
            }
        }
        Event::KillFocus => game.held.clear(),
        Event::MouseDown { x, y, button, .. } => {
            after.focus = true;
            if button == MouseButton::Left && teleport(game, bounds, scale, x, y) {
                return None;
            }
            if matches!(button, MouseButton::Left | MouseButton::Right) {
                after.capture = true;
                drag.set(Some(Drag { button, x, y }));
            }
        }
        Event::MouseMove { x, y, .. } => {
            let (Some(d), Some(run)) = (drag.get(), game.run.as_mut()) else {
                return None;
            };
            let (dx, dy) = ((x - d.x) as f32, (y - d.y) as f32);
            if d.button == MouseButton::Left {
                run.flyer.look(dx, dy);
            } else {
                run.flyer.pan(dx, dy);
            }
            run.moved();
            drag.set(Some(Drag { x, y, ..d }));
        }
        Event::MouseUp { .. } => {
            drag.set(None);
            after.release = true;
        }
        Event::CaptureChanged => drag.set(None),
        Event::MouseWheel {
            delta,
            horizontal: false,
            ..
        } => {
            if let Some(run) = game.run.as_mut() {
                run.flyer.wheel(f32::from(delta));
                after.invalidate = true;
            }
        }
        _ => {}
    }
    None
}

/// A key that is not a movement.
fn command(game: &mut Game, key: Key) {
    match key {
        k if k == Key::H => game.show_help = !game.show_help,
        k if k == Key::M => game.show_map = !game.show_map,
        k if k == Key::G => {
            let seed = game.seed().wrapping_add(1);
            game.regenerate(seed);
        }
        _ => {}
    }
    let Some(run) = game.run.as_mut() else {
        return;
    };
    let holes = run.scene.course.holes.len();
    let eye = run.flyer.camera.eye;
    let here = usize::from(run.scene.course.hole_of.at(eye.x, eye.z));
    let target = match key {
        k if k == Key::N => Some(if here < holes { (here + 1) % holes } else { 0 }),
        k if k == Key::P => Some(if here < holes {
            (here + holes - 1) % holes
        } else {
            holes - 1
        }),
        k if k == Key::HOME => Some(0),
        k if k == Key::T => {
            run.authentic = !run.authentic;
            None
        }
        k if k == Key::R => {
            run.cycle_scale();
            None
        }
        k if k == Key::B => {
            run.start_bench();
            None
        }
        _ => None,
    };
    if let Some(n) = target {
        let bake = &run.scene.bake;
        let hole = &run.scene.course.holes[n];
        run.flyer.to_tee(hole, |x, z| bake.height_at(x, z));
        run.moved();
    }
}

/// A click on the overhead map flies the camera above that spot.
fn teleport(game: &mut Game, b: Rect, s: i32, x: i32, y: i32) -> bool {
    if !game.show_map {
        return false;
    }
    let Some(run) = game.run.as_mut() else {
        return false;
    };
    let size = run.minimap.width() as i32;
    let r = hud::minimap_rect(Rect::new(0, 0, b.width(), b.height()), size, s);
    if x < r.left || y < r.top || x >= r.right || y >= r.bottom {
        return false;
    }
    let k = run.scene.bake.size as f32 / size as f32;
    let (wx, wz) = ((x - r.left) as f32 * k, (y - r.top) as f32 * k);
    let ground = run.scene.bake.height_at(wx, wz);
    run.flyer.camera.eye = Vec3::new(wx, ground + 60.0 + EYE, wz);
    run.flyer.camera.pitch = -0.4;
    run.moved();
    true
}

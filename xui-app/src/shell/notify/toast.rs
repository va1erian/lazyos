//! The toast windows: one chromeless panel per toast, created when the
//! model gives it a place, moved with `PlaceSurface` while its tween runs,
//! redrawn only when what it paints changes, and closed when the model has
//! no frame for it any more.
//!
//! A toast never takes keyboard focus (panels are never focused and have no
//! keyboard session): its actions, its close mark and its dismiss are
//! pointer-driven. Hovering pauses its timeout; a click on the body tells
//! the sender; a sideways drag past [`FLICK`] dismisses it. Input goes to the
//! model (`Center`), which decides; the next heartbeat acts on the events.

use std::cell::RefCell;
use std::rc::Rc;

use lazymotion::Motion;
use lazyshell::notify::card::{Card, Hit, FLICK};
use lazyshell::notify::frame::{Frame, Look, Stage};
use lazyshell::notify::note::{Level, Note, Source};
use xui_core::app::{App, Ui, WindowHandle};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec, TextStyle};
use xui_core::image::Image;
use xui_core::{Control, Dip, MouseButton};

use super::super::ctx::Ctx;
use super::super::theme::color;
use super::paint::{self, Picture};
use crate::client_window::SurfaceRole;
use crate::probe;

/// What a toast window paints, shared with the presenter that updates it.
pub struct View {
    pub id: u64,
    pub level: Level,
    pub card: Card,
    pub picture: Picture,
    pub frame: Frame,
    pub motion: Motion,
    /// What the pointer is over.
    pub hit: Option<Hit>,
    /// The x (screen pixels) where the button went down, for the flick.
    pub press: Option<i32>,
}

/// A window message.
pub enum ToastMsg {
    Move(i32, i32),
    Leave,
    Down(i32),
    Up(i32, i32),
    Repaint,
}

/// One toast window and what the presenter knows of it.
pub struct Presented {
    pub id: u64,
    pub rev: u32,
    handle: WindowHandle<ToastMsg>,
    view: Rc<RefCell<View>>,
    surface: u64,
    /// The top-left it was last placed at, design pixels.
    placed: (i32, i32),
    /// What it last painted.
    look: Look,
    /// Where the probe last printed it (screen pixels).
    probed: Option<(i32, i32)>,
    height: i32,
}

impl Presented {
    fn close(&self) {
        self.handle.close();
    }
}

/// A freshly measured toast: its card and the model's revision it is for.
struct Fresh {
    id: u64,
    rev: u32,
    card: Card,
    level: Level,
    icon: Option<Source>,
}

/// Bring the windows in line with the model at `now`: measure what is new,
/// lay the stack out, then create, place, repaint and close windows.
pub fn sync<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, now: u64) {
    let state = &ctx.notify;
    if state.center.borrow().is_idle() && state.windows.borrow().is_empty() {
        return;
    }
    let fresh = measure(ctx, ui);
    let motion = {
        let mut center = state.center.borrow_mut();
        center.layout(super::bar_top(ctx), now);
        center.motion()
    };
    let frames = state.center.borrow().frames(ctx.screen.0, now);
    let s = ctx.scale();

    // Windows whose toast has no frame any more.
    state.windows.borrow_mut().retain(|window| {
        let keep = frames.iter().any(|frame| frame.id == window.id);
        if !keep {
            window.close();
        }
        keep
    });

    for frame in frames {
        let index = state
            .windows
            .borrow()
            .iter()
            .position(|window| window.id == frame.id);
        let fresh_card = fresh.iter().find(|f| f.id == frame.id);
        match (index, fresh_card) {
            (Some(index), Some(card)) => update(ctx, ui, index, card, &frame, motion),
            (Some(index), None) => refresh(ctx, index, &frame, motion, s),
            (None, Some(card)) if frame.stage != Stage::Leaving => {
                create(ctx, ui, card, &frame, motion, s);
            }
            (None, _) => {}
        }
    }
    probe_widgets(ctx);
}

/// Cards for the live toasts the windows do not show at their revision.
fn measure<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) -> Vec<Fresh> {
    let state = &ctx.notify;
    let pending: Vec<(u64, u32, String, Note)> = {
        let windows = state.windows.borrow();
        state
            .center
            .borrow()
            .live()
            .iter()
            .filter(|toast| {
                windows
                    .iter()
                    .find(|window| window.id == toast.id)
                    .is_none_or(|window| window.rev != toast.rev)
            })
            .map(|toast| (toast.id, toast.rev, toast.owner.clone(), toast.note.clone()))
            .collect()
    };
    let s = ctx.scale();
    let measure_text = |text: &str, bold: bool| {
        let mut style = TextStyle::new(color(0), Dip(if bold { 12.0 } else { 11.0 }));
        if bold {
            style = style.bold();
        }
        (ui.measure_text(text, &style, ui.dpi()).width + s - 1) / s
    };
    let mut fresh = Vec::with_capacity(pending.len());
    for (id, rev, owner, note) in pending {
        let name = state.display_name(ctx, &owner);
        let card = Card::new(&note, &name, &measure_text);
        state.center.borrow_mut().set_height(id, rev, card.height);
        fresh.push(Fresh {
            id,
            rev,
            card,
            level: note.level,
            icon: note.icon.clone(),
        });
    }
    fresh
}

/// The picture a toast shows.
fn picture(icon: Option<&Source>) -> Picture {
    match icon {
        Some(Source::Lucide(name)) => {
            lazyicons::from_name(name).map_or(Picture::Level, Picture::Outline)
        }
        Some(Source::Image(image)) => {
            Image::from_rgba(image.width, image.height, image.data.clone())
                .map_or(Picture::Level, |image| Picture::Image(Rc::new(image)))
        }
        None => Picture::Level,
    }
}

fn create<M: 'static>(
    ctx: &Rc<Ctx>,
    ui: &Ui<M>,
    fresh: &Fresh,
    frame: &Frame,
    motion: Motion,
    s: i32,
) {
    let (x, y) = (frame.rect.x, frame.rect.y);
    let view = Rc::new(RefCell::new(View {
        id: fresh.id,
        level: fresh.level,
        card: fresh.card.clone(),
        picture: picture(fresh.icon.as_ref()),
        frame: frame.clone(),
        motion,
        hit: None,
        press: None,
    }));
    ctx.backend
        .set_next_role(SurfaceRole::Panel { x: x * s, y: y * s });
    let spec = PlatformSpec::new("LazyOS")
        .size(Dip(fresh.card.width as f32), Dip(fresh.card.height as f32));
    let built = Rc::clone(ctx);
    let shared = Rc::clone(&view);
    match ui.open_window(spec, move |ui| ToastApp::build(built, shared, ui)) {
        Ok(handle) => {
            let surface = ctx.backend.surface_of(handle.window()).unwrap_or(0);
            ctx.notify.windows.borrow_mut().push(Presented {
                id: fresh.id,
                rev: fresh.rev,
                handle,
                view,
                surface,
                placed: (x, y),
                look: frame.look(),
                probed: None,
                height: fresh.card.height,
            });
        }
        Err(error) => ctx.note("notify-window", || {
            format!("SHELL:NOTIFY:WINDOW:FAIL {error}")
        }),
    }
}

/// A toast whose note changed: same size repaints in place, a different one
/// gets a new window (a surface's size is fixed).
fn update<M: 'static>(
    ctx: &Rc<Ctx>,
    ui: &Ui<M>,
    index: usize,
    fresh: &Fresh,
    frame: &Frame,
    motion: Motion,
) {
    let same_size = ctx.notify.windows.borrow()[index].height == fresh.card.height;
    if !same_size {
        let old = ctx.notify.windows.borrow_mut().remove(index);
        old.close();
        create(ctx, ui, fresh, frame, motion, ctx.scale());
        return;
    }
    let mut windows = ctx.notify.windows.borrow_mut();
    let window = &mut windows[index];
    window.rev = fresh.rev;
    {
        let mut view = window.view.borrow_mut();
        view.card = fresh.card.clone();
        view.level = fresh.level;
        view.picture = picture(fresh.icon.as_ref());
    }
    window.probed = None;
    // Force a repaint with the new content.
    window.look = Look {
        hovered: !frame.look().hovered,
        ..frame.look()
    };
    drop(windows);
    refresh(ctx, index, frame, motion, ctx.scale());
}

/// Move and repaint window `index` for `frame`.
fn refresh(ctx: &Ctx, index: usize, frame: &Frame, motion: Motion, s: i32) {
    let mut windows = ctx.notify.windows.borrow_mut();
    let window = &mut windows[index];
    if window.placed != (frame.rect.x, frame.rect.y) && window.surface != 0 {
        if let Err(code) =
            ctx.client
                .place_surface(window.surface, frame.rect.x * s, frame.rect.y * s)
        {
            ctx.note("notify-place", || {
                format!("SHELL:NOTIFY:PLACE:FAIL err={}", -code)
            });
        }
        window.placed = (frame.rect.x, frame.rect.y);
    }
    {
        let mut view = window.view.borrow_mut();
        view.motion = motion;
        view.frame = frame.clone();
    }
    let look = frame.look();
    if look != window.look {
        window.look = look;
        window.handle.send(ToastMsg::Repaint);
    }
}

/// Print each settled toast's actions and close mark for the UI probe
/// (screen pixels), again when it has been moved.
fn probe_widgets(ctx: &Ctx) {
    if !probe::enabled() {
        return;
    }
    let s = ctx.scale();
    let mut windows = ctx.notify.windows.borrow_mut();
    for window in windows.iter_mut() {
        let view = window.view.borrow();
        if view.frame.stage != Stage::Shown || view.frame.moving {
            continue;
        }
        let origin = (view.frame.rect.x * s, view.frame.rect.y * s);
        if window.probed == Some(origin) {
            continue;
        }
        // The newest toast is the window `toast`, its controls the widgets
        // `retry`, `dismiss`, ...; `toast:<label>` names the same rectangles
        // as screen rectangles, and `toast:<id>:<label>` one toast's own.
        probe::rect(
            "window:toast",
            origin.0,
            origin.1,
            view.card.width * s,
            view.card.height * s,
        );
        let name = |label: &str, r: lazyshell::Rect| {
            let (x, y, w, h) = (origin.0 + r.x * s, origin.1 + r.y * s, r.w * s, r.h * s);
            probe::rect(&format!("toast:{label}"), x, y, w, h);
            probe::rect(&format!("toast:{}:{label}", view.id), x, y, w, h);
            probe::widget("toast", label, r.x * s, r.y * s, w, h);
        };
        for button in &view.card.actions {
            name(&button.label.to_lowercase(), button.rect);
        }
        name("dismiss", view.card.close);
        name(
            "body",
            lazyshell::Rect::new(0, 0, view.card.width, view.card.height),
        );
        window.probed = Some(origin);
    }
}

/// The toast window's app.
struct ToastApp {
    ctx: Rc<Ctx>,
    view: Rc<RefCell<View>>,
    root: Control<ToastMsg>,
}

impl ToastApp {
    fn build(ctx: Rc<Ctx>, view: Rc<RefCell<View>>, ui: &mut Ui<ToastMsg>) -> ToastApp {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("toast node");
        {
            let (ctx, view) = (Rc::clone(&ctx), Rc::clone(&view));
            root.set_painter(Rc::new(move |canvas| {
                paint::paint(canvas, &ctx, &view.borrow());
            }));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(ToastMsg::Move(x, y)),
            Event::MouseLeave => Some(ToastMsg::Leave),
            Event::MouseDown {
                x,
                button: MouseButton::Left,
                ..
            } => Some(ToastMsg::Down(x)),
            Event::MouseUp {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => Some(ToastMsg::Up(x, y)),
            _ => None,
        });
        ToastApp { ctx, view, root }
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (x, y) = self.ctx.to_design(x, y);
        self.view.borrow().card.hit(x, y)
    }

    /// Tell the model the pointer is on (or off) this toast.
    fn hovering(&self, on: bool) {
        let id = self.view.borrow().id;
        let now = crate::sys::clock_ticks();
        self.ctx.notify.center.borrow_mut().hover(id, on, now);
    }
}

impl App for ToastApp {
    type Msg = ToastMsg;

    fn update(&mut self, msg: ToastMsg, _ui: &mut Ui<ToastMsg>) {
        let id = self.view.borrow().id;
        let now = crate::sys::clock_ticks();
        let s = self.ctx.scale();
        match msg {
            ToastMsg::Repaint => self.root.invalidate(),
            ToastMsg::Move(x, y) => {
                let hit = self.hit(x, y);
                let (was, press) = {
                    let view = self.view.borrow();
                    (view.hit, view.press)
                };
                if was.is_none() && hit.is_some() {
                    self.hovering(true);
                }
                // A drag sideways past the flick distance dismisses it.
                if press.is_some_and(|from| (x - from).abs() >= FLICK * s) {
                    self.view.borrow_mut().press = None;
                    self.ctx.notify.center.borrow_mut().dismiss(id, now);
                    return;
                }
                if was != hit {
                    self.view.borrow_mut().hit = hit;
                    self.root.invalidate();
                }
            }
            ToastMsg::Leave => {
                let was = self.view.borrow_mut().hit.take();
                self.view.borrow_mut().press = None;
                if was.is_some() {
                    self.hovering(false);
                    self.root.invalidate();
                }
            }
            ToastMsg::Down(x) => self.view.borrow_mut().press = Some(x),
            ToastMsg::Up(x, y) => {
                let press = self.view.borrow_mut().press.take();
                // The drag was a flick (handled on the move), or the press
                // began elsewhere: only a click on a thing counts.
                if press.is_none_or(|from| (x - from).abs() >= FLICK * s) {
                    return;
                }
                let mut center = self.ctx.notify.center.borrow_mut();
                match self.hit(x, y) {
                    Some(Hit::Close) => center.dismiss(id, now),
                    Some(Hit::Action(action)) => center.invoke(id, action, now),
                    Some(Hit::Body) => center.activate(id, now),
                    None => {}
                }
            }
        }
    }
}

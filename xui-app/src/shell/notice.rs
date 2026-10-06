//! The "app stopped" notice window (issue #549).
//!
//! When `init` gives up on an app this session opened ([`super::failures`]),
//! the shell opens one small decorated window: the app's name, what happened,
//! the reason the app reported, and *Restart* / *Close*. The text and
//! geometry are `lazyshell::notice` (host-tested); this module measures,
//! paints and acts. One notice shows at a time; failures that arrive while it
//! is open wait their turn, so a burst never stacks windows.
//!
//! Serial: `SHELL:NOTICE:OPEN app=<id> title=<title>`, `SHELL:NOTICE:CLOSE
//! app=<id>`, `SHELL:NOTICE:RESTART app=<id>`; with the UI probe, the buttons
//! as `UI:WIDGET name=restart|close window=<title>`.

use std::rc::Rc;

use lazyshell::notice::{Button, Failure, Layout};
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec, TextStyle};
use xui_core::theme::look;
use xui_core::{Canvas, Control, Dip, MouseButton, Rect};

use super::ctx::Ctx;
use super::theme::{chrome_look, color};
use crate::client_window::SurfaceRole;
use crate::probe;

/// Body text size.
const TEXT: Dip = Dip(12.0);
/// Height of one text line (design pixels).
const LINE_H: i32 = 18;

/// A notice window message.
pub enum NoticeMsg {
    Move(i32, i32),
    Press(i32, i32),
}

/// The notice on screen.
pub struct Showing {
    pub failure: Failure,
    pub layout: Layout,
    pub hover: Option<Button>,
}

/// Queue `failure` and show it when no other notice is open.
pub fn post<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, failure: Failure) {
    ctx.pending_notices.borrow_mut().push_back(failure);
    show_next(ctx, ui);
}

/// Forget a notice the user closed with the title bar, then show the next.
pub fn pump<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    let gone = ctx
        .notice_window
        .borrow()
        .as_ref()
        .is_some_and(|handle| !handle.is_open());
    if gone {
        ctx.notice_window.borrow_mut().take();
        if let Some(showing) = ctx.notice.borrow_mut().take() {
            println!("SHELL:NOTICE:CLOSE app={}", showing.failure.app);
        }
    }
    show_next(ctx, ui);
}

/// Open the next queued notice, if none is open.
fn show_next<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    if ctx.notice_window.borrow().is_some() {
        return;
    }
    let Some(failure) = ctx.pending_notices.borrow_mut().pop_front() else {
        return;
    };
    let s = ctx.scale();
    let measure = |text: &str| {
        let width = ui
            .measure_text(text, &TextStyle::new(color(0), TEXT), ui.dpi())
            .width;
        (width + s - 1) / s
    };
    let layout = Layout::new(&failure, LINE_H, &measure);
    let title = failure.title();
    ctx.backend.set_next_role(SurfaceRole::Window);
    let spec = PlatformSpec::new(&title).size(Dip(layout.width as f32), Dip(layout.height as f32));
    let (app, buttons) = (failure.app.clone(), layout.clone());
    *ctx.notice.borrow_mut() = Some(Showing {
        failure,
        layout,
        hover: None,
    });
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| NoticeApp::build(built, ui)) {
        Ok(handle) => {
            *ctx.notice_window.borrow_mut() = Some(handle);
            println!("SHELL:NOTICE:OPEN app={app} title={title}");
            for button in [Button::Restart, Button::Close] {
                let r = buttons.button(button);
                probe::widget(
                    &title,
                    button.probe_name(),
                    r.x * s,
                    r.y * s,
                    r.w * s,
                    r.h * s,
                );
            }
        }
        Err(error) => {
            ctx.notice.borrow_mut().take();
            ctx.note("notice-open", || format!("SHELL:NOTICE:FAIL {error}"));
        }
    }
}

/// Close the open notice (a button was pressed).
fn close(ctx: &Ctx) {
    if let Some(handle) = ctx.notice_window.borrow_mut().take() {
        handle.close();
    }
    if let Some(showing) = ctx.notice.borrow_mut().take() {
        println!("SHELL:NOTICE:CLOSE app={}", showing.failure.app);
    }
}

/// The notice window's app.
pub struct NoticeApp {
    ctx: Rc<Ctx>,
    root: Control<NoticeMsg>,
}

impl NoticeApp {
    fn build(ctx: Rc<Ctx>, ui: &mut Ui<NoticeMsg>) -> NoticeApp {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("notice node");
        {
            let ctx = Rc::clone(&ctx);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &ctx)));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(NoticeMsg::Move(x, y)),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => Some(NoticeMsg::Press(x, y)),
            _ => None,
        });
        NoticeApp { ctx, root }
    }

    /// The button under screen point `(x, y)` of the window.
    fn hit(&self, x: i32, y: i32) -> Option<Button> {
        let (x, y) = self.ctx.to_design(x, y);
        self.ctx.notice.borrow().as_ref()?.layout.hit(x, y)
    }
}

impl App for NoticeApp {
    type Msg = NoticeMsg;

    fn update(&mut self, msg: NoticeMsg, _ui: &mut Ui<NoticeMsg>) {
        match msg {
            NoticeMsg::Move(x, y) => {
                let hover = self.hit(x, y);
                let changed = match self.ctx.notice.borrow_mut().as_mut() {
                    Some(showing) if showing.hover != hover => {
                        showing.hover = hover;
                        true
                    }
                    _ => false,
                };
                if changed {
                    self.root.invalidate();
                }
            }
            NoticeMsg::Press(x, y) => match self.hit(x, y) {
                Some(Button::Restart) => {
                    let app = self
                        .ctx
                        .notice
                        .borrow()
                        .as_ref()
                        .map(|s| s.failure.app.clone());
                    close(&self.ctx);
                    if let Some(app) = app {
                        println!("SHELL:NOTICE:RESTART app={app}");
                        let _ = self.ctx.launch(&app, None);
                    }
                }
                Some(Button::Close) => close(&self.ctx),
                None => {}
            },
        }
    }
}

/// Paint the notice: the panel colours, the text lines, the two buttons.
fn paint(canvas: &mut dyn Canvas, ctx: &Ctx) {
    let palette = ctx.theme.borrow().palette();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let s = ctx.scale();
    let bounds = canvas.bounds();
    canvas.fill_rect(bounds, color(palette.overlay_bg));
    let notice = ctx.notice.borrow();
    let Some(showing) = notice.as_ref() else {
        return;
    };
    let rect = |r: lazyshell::Rect| Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s);
    let text_w = showing.layout.width - 2 * lazyshell::notice::PAD;
    for line in &showing.layout.lines {
        let area = rect(lazyshell::Rect::new(
            lazyshell::notice::PAD,
            line.y,
            text_w,
            LINE_H,
        ));
        let mut style = TextStyle::new(color(palette.overlay_text), TEXT).middle();
        if line.heading {
            style = style.bold();
        }
        canvas.draw_text(&line.text, area, &style);
    }
    for button in [Button::Restart, Button::Close] {
        let area = rect(showing.layout.button(button));
        let lit = showing.hover == Some(button);
        let face = if lit || button == Button::Restart {
            palette.overlay_selected
        } else {
            uitheme::mix(palette.overlay_bg, palette.overlay_text, 1, 6)
        };
        look::face(canvas, area, 4.0 * s as f32, color(face), &deco);
        let ink = if lit || button == Button::Restart {
            uitheme::text_on(palette.overlay_selected)
        } else {
            palette.overlay_text
        };
        let style = TextStyle::new(color(ink), TEXT).centered().middle();
        canvas.draw_text(button.label(), area, &style);
    }
}

//! Desktop notifications (docs/notify-plan.md, issue #747): LazyShell serves
//! `os.lazy.notify` and draws the toasts. The decisions (validation, who may
//! post and as whom, rate limits, placement, coalescing, timeouts, the
//! motion curves) live in the host-tested `lazyshell::notify`; this module
//! wires them to Messenger and the compositor.
//!
//! * [`service`]: register, authorize, identify the caller, apply.
//! * [`listen`]: the apps' event channels (`Activated`, `ActionInvoked`,
//!   `Closed`), pinged for liveness.
//! * [`toast`]: one panel window per toast, placed and redrawn only while
//!   something moves; clicks, hover and flicks come back through the model.
//! * [`paint`]: a toast's face.
//!
//! Nothing is kept: there is no history and no way to list what was shown.
//! The shell's own notice, "app stopped" ([`post_failure`]), is one more
//! toast from the reserved system owner, with *Restart* and *Close*.
//!
//! Serial: `SHELL:NOTIFY:SERVICE:PASS`, `SHELL:NOTIFY:POST owner=<key>
//! id=<n> level=<l> title=<t>`, `SHELL:NOTIFY:SHOW id=<n>`, `SHELL:NOTIFY:
//! CLOSE id=<n> reason=<r>`, `SHELL:NOTIFY:ACTION id=<n> action=<n>`,
//! `SHELL:NOTIFY:DENY uid=<n> label=<id> sender=<task> why=<...>`, `SHELL:NOTIFY:LISTEN
//! owner=<key>`, `SHELL:NOTIFY:SETTINGS motion=<m> dnd=<0|1> muted=<n>`;
//! and for the app-stopped notice `SHELL:NOTICE:OPEN app=<id> title=<t>`,
//! `SHELL:NOTICE:RESTART app=<id>`, `SHELL:NOTICE:CLOSE app=<id>`. With the
//! UI probe each action and the close mark print as `UI:WIDGET
//! name=<label|dismiss> window=<title>` once the toast has settled.

mod feed;
mod listen;
mod paint;
mod service;
mod toast;

use std::cell::RefCell;
use std::rc::Rc;

use lazyshell::notice::{Failure, ACTION_CLOSE, ACTION_RESTART};
use lazyshell::notify::policy::{SCRIPT_NAME, SCRIPT_OWNER, SYSTEM_NAME, SYSTEM_OWNER};
use lazyshell::notify::{Center, EventKind};
use lazyshell::taskbar::BAR_H;
use xui_core::app::Ui;

use super::ctx::Ctx;
use crate::sys;

/// The notification center's state, shared through [`Ctx`].
pub struct NotifyState {
    /// The model: the queue, the timers and the tweens.
    pub center: RefCell<Center>,
    service: RefCell<service::NotifyService>,
    listeners: RefCell<listen::Listeners>,
    windows: RefCell<Vec<toast::Presented>>,
    settings: RefCell<feed::Feed>,
    /// The login session the shell serves; `None` refuses everyone.
    pub session: Option<u64>,
    /// The app-stopped notices on screen: toast id and the app's id.
    notices: RefCell<Vec<(u64, String)>>,
}

impl NotifyState {
    pub fn new(session: Option<u64>) -> NotifyState {
        NotifyState {
            center: RefCell::new(Center::new()),
            service: RefCell::new(service::NotifyService::default()),
            listeners: RefCell::new(listen::Listeners::default()),
            windows: RefCell::new(Vec::new()),
            settings: RefCell::new(feed::Feed::new()),
            session,
            notices: RefCell::new(Vec::new()),
        }
    }

    /// The name a toast's header shows for `owner`: the registry's name for
    /// the kernel-identified app, never text from the sender.
    pub fn display_name(&self, ctx: &Ctx, owner: &str) -> String {
        match owner {
            SYSTEM_OWNER => SYSTEM_NAME.to_owned(),
            SCRIPT_OWNER => SCRIPT_NAME.to_owned(),
            app => ctx.tray.name(app),
        }
    }
}

/// Show the "app stopped" notice for `failure` (issues #549, #747).
pub fn post_failure(ctx: &Ctx, failure: Failure) {
    let now = sys::clock_ticks();
    let title = failure.title();
    let id = ctx
        .notify
        .center
        .borrow_mut()
        .post_system(failure.request(), now);
    println!("SHELL:NOTICE:OPEN app={} title={title}", failure.app);
    ctx.notify.notices.borrow_mut().push((id, failure.app));
}

/// One heartbeat: settings, requests, liveness, time, windows, events.
pub fn pump<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    let state = &ctx.notify;
    state.settings.borrow_mut().poll(ctx);
    state.service.borrow_mut().pump(ctx);
    state.listeners.borrow_mut().ping(ctx);
    let now = sys::clock_ticks();
    {
        let mut center = state.center.borrow_mut();
        center.tick(now);
    }
    toast::sync(ctx, ui, now);
    handle_events(ctx, ui);
}

/// What happened to toasts since the last look: tell the senders, run the
/// shell's own notice actions, pulse the tray item a toast lands in.
fn handle_events<M: 'static>(ctx: &Rc<Ctx>, _ui: &Ui<M>) {
    let events = ctx.notify.center.borrow_mut().take_events();
    for event in events {
        match event.kind {
            EventKind::Shown => {
                println!("SHELL:NOTIFY:SHOW id={}", event.id);
                super::tray::landed(ctx, &event.owner);
            }
            EventKind::Closed(reason) => {
                println!(
                    "SHELL:NOTIFY:CLOSE id={} reason={}",
                    event.id,
                    reason.as_str()
                );
            }
            EventKind::Action(action) => {
                println!("SHELL:NOTIFY:ACTION id={} action={action}", event.id);
            }
            EventKind::Activated => {
                // The toast belongs to an app: its windows come forward (the shell's own
                // notices bring nothing).
                if event.owner != SYSTEM_OWNER {
                    let app = ctx.notify.listeners.borrow().app_of(&event.owner);
                    if let Some((label, session)) = app {
                        let _ = ctx.client.activate_app(label, session);
                    }
                }
            }
        }
        if event.owner == SYSTEM_OWNER {
            notice_event(ctx, event.id, event.kind);
        } else {
            ctx.notify.listeners.borrow().deliver(ctx, &event);
        }
    }
}

/// The shell's own notice: *Restart* launches the app again, and any end of
/// the toast closes the notice.
fn notice_event(ctx: &Ctx, id: u64, kind: EventKind) {
    let app = ctx
        .notify
        .notices
        .borrow()
        .iter()
        .find(|(notice, _)| *notice == id)
        .map(|(_, app)| app.clone());
    let Some(app) = app else {
        return;
    };
    match kind {
        EventKind::Action(ACTION_RESTART) => {
            println!("SHELL:NOTICE:RESTART app={app}");
            let _ = ctx.launch(&app, None);
        }
        EventKind::Action(ACTION_CLOSE) => {}
        EventKind::Closed(_) => {
            ctx.notify
                .notices
                .borrow_mut()
                .retain(|(notice, _)| *notice != id);
            println!("SHELL:NOTICE:CLOSE app={app}");
        }
        _ => {}
    }
}

/// The taskbar's top edge in design pixels: the stack sits above it.
pub fn bar_top(ctx: &Ctx) -> i32 {
    ctx.screen.1 - BAR_H
}

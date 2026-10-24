#![allow(dead_code)]
//! The rig of the page view's tests: the view alone in a window, between a strip the height of a
//! toolbar and one the height of a status bar, driven offscreen against a recording engine. It does what any window does with the
//! mailbox (`show_frame` and `show_state`) and nothing else, so the tests judge the view, the
//! damage rules and the mailbox, not a window's chrome (LazyWeb's own tests are
//! `xui-app/lazyweb/tests`).

use std::rc::Rc;
use std::sync::Arc;

use xui_core::app::{App, Ui};
use xui_core::arrange::{build, column, label, LayoutExt};
use xui_core::backend::Result;
use xui_core::Dip;
use xui_webkit::engine::Engine;
use xui_webkit::mailbox::{Mailbox, Notice};
use xui_webkit::view::{Hooks, PageView};

/// The strips above and below the view, in device-independent pixels.
pub const STRIP: f32 = 38.0;
pub const STATUS: f32 = 24.0;

#[derive(Clone, Debug)]
pub enum Msg {
    /// The engine painted a frame.
    Frame,
    /// The engine's page state changed.
    State,
    /// The page was clicked.
    Clicked,
    /// The page asked for this pointer shape.
    Cursor(xui_webkit::events::CursorKind),
}

thread_local! {
    /// What the view's `swallow_right` hook answers (a test sets it).
    static SWALLOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the window's menu take (or leave to the page) the next right presses.
pub fn swallow_right(on: bool) {
    SWALLOW.with(|s| s.set(on));
}

/// What a window adds to the view; the rig adds nothing.
pub struct Host;

impl Host {
    pub fn none() -> Host {
        Host
    }
}

pub struct Browser {
    page: Rc<PageView<Msg>>,
    mailbox: Arc<Mailbox>,
    spare: Vec<u8>,
    crashed: bool,
    /// Documents committed, as LazyWeb follows them: a new one is `PageView::page_committed`.
    commits: u32,
}

impl Browser {
    pub fn build(
        ui: &mut Ui<Msg>,
        engine: Rc<dyn Engine>,
        mailbox: Arc<Mailbox>,
        _first: Option<String>,
        _host: Host,
    ) -> Result<Browser> {
        let page = xui_core::arrange::Handle::<PageView<Msg>>::default();
        ui.root(
            column().children((
                label("").fixed(Dip(STRIP)),
                build(move |ui| {
                    let hooks = Hooks {
                        is_shortcut: Rc::new(|_, _| false),
                        clicked: Msg::Clicked,
                        right_click: None,
                        swallow_right: Some(Rc::new(|| SWALLOW.with(std::cell::Cell::get))),
                    };
                    PageView::new(ui, engine, hooks)
                })
                .bind(&page)
                .fill(1),
                label("").fixed(Dip(STATUS)),
            )),
        )?;
        let proxy = ui.proxy();
        mailbox.set_notify(move |notice| {
            let _ = proxy.send(match notice {
                Notice::Frame => Msg::Frame,
                Notice::State => Msg::State,
                Notice::Events => return,
            });
        });
        let page = page.get();
        page.focus();
        Ok(Browser {
            page,
            mailbox,
            spare: Vec::new(),
            crashed: false,
            commits: 0,
        })
    }

    pub fn page(&self) -> &Rc<PageView<Msg>> {
        &self.page
    }
}

impl App for Browser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => {
                self.page.add_bands(xui_webkit::bands::take());
                if let Some(update) = self.mailbox.take_update(std::mem::take(&mut self.spare)) {
                    if self.crashed {
                        self.spare = update.full.map(|frame| frame.rgba).unwrap_or_default();
                        return;
                    }
                    let applied = self.page.apply(update);
                    if applied.resync {
                        self.mailbox.request_full();
                    }
                    if !applied.spare.is_empty() {
                        self.spare = applied.spare;
                    }
                }
            }
            Msg::State => {
                let Some(state) = self.mailbox.take_state() else {
                    return;
                };
                self.crashed = state.crashed;
                if state.crashed {
                    self.page.clear();
                }
                if state.commits != self.commits {
                    self.commits = state.commits;
                    self.page.page_committed();
                    self.page.forget_bands();
                }
            }
            Msg::Clicked => {}
            Msg::Cursor(kind) => self.page.set_cursor(kind),
        }
    }
}

//! The timers, the UI probe and shutting down.
//!
//! The only periodic work is the Live tab's registry read (a kernel read,
//! every [`TICK_MS`]). The doorbell look-out and the drain continuation belong
//! to the Topic monitor window and are armed only while a subscription needs
//! them ([`crate::topics`]).

use xui_app::fabric;
use xui_app::probe;
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::geometry::Rect;

use crate::app::Playground;
use crate::msg::Msg;

/// The registry refresh period.
pub const TICK_MS: u32 = 2000;
/// A probe line is printed this long after the layout last changed.
const PROBE_SETTLE_MS: u32 = 250;
/// The height of a `Tabs` header, in design pixels (xui's `tabs.rs`).
const TAB_BAR: i32 = 32;
/// A list's column-title row and a row of a list or tree, in design pixels (xui's
/// `ListView` and `TreeView`), and how many rows get a probe name.
const LIST_HEADER: i32 = 22;
const ROW_HEIGHT: i32 = 22;
const PROBE_ROWS: i32 = 6;

impl Playground {
    /// Read the registry and refresh the Live tab and the LIVE badges.
    pub fn tick(&mut self, _ui: &mut Ui<Msg>) {
        // A dragged layout settles within a tick or two; write it then.
        self.save_panes();
        match fabric::registry() {
            Ok(entries) => {
                self.last_live_ms = self.now_ms();
                if self.ex.set_registry(&entries) {
                    self.refill_tree();
                }
                self.refill_live();
                // The monitor names event publishers from the same read.
                self.topics.send(crate::tmsg::TMsg::Registry(self.registry_snapshot()));
            }
            Err(code) => self.note(&format!("registry: {}", fabric::errno_text(code))),
        }
    }

    /// Per-message housekeeping: counters, and the probe timer.
    pub fn after(&mut self, ui: &mut Ui<Msg>) {
        self.apply_reveal(ui);
        self.update_status();
        if self.probe_due.replace(false) && self.timers.probe.get().is_none() {
            self.timers.probe.set(Some(ui.set_timer(PROBE_SETTLE_MS)));
        }
    }

    /// The strip's "x": hide the notice in both windows.
    pub fn notice_dismiss(&mut self) {
        if let Some(note) = crate::notice::dismiss(&self.notice) {
            self.note(&note);
        }
        self.notice_changed();
        self.topics.send(crate::tmsg::TMsg::NoticeChanged);
    }

    /// View > Show developer-tool notice: show it in both windows, forget the choice.
    pub fn notice_show(&mut self) {
        if let Some(note) = crate::notice::show(&self.notice) {
            self.note(&note);
        }
        self.notice_changed();
        self.topics.send(crate::tmsg::TMsg::NoticeChanged);
    }

    /// The shared notice changed (here or in the monitor): re-lay the strip out.
    pub fn notice_changed(&mut self) {
        self.w.strip.get().sync();
        self.probe_soon();
    }

    pub fn probe_soon(&self) {
        self.probe_due.set(true);
    }

    /// The status bar's first part: the last thing worth saying.
    pub fn note(&self, text: &str) {
        self.w.status.get().set_text(0, text);
    }

    /// Status bar parts 1 and 2: live runs and the Topic monitor.
    pub fn update_status(&self) {
        let status = self.w.status.get();
        status.set_text(1, &format!("runs live: {}", self.live_runs()));
        let text = crate::topicshost::status_text(self.topics.is_open(), self.topics.listening);
        status.set_text(2, &text);
    }

    /// The settle timer fired: print where the named controls are.
    pub fn probe_now(&mut self, ui: &mut Ui<Msg>) {
        if let Some(id) = self.timers.probe.take() {
            ui.kill_timer(id);
        }
        if !probe::enabled() {
            return;
        }
        let scale = ui.dpi() as i32 / 96;
        let title = self.title.clone();
        // The editor and the Output keep stale rectangles while the row is hidden.
        let hidden = !self.bottom_shown();
        let say = |name: &str, r: Rect| {
            let in_bottom = matches!(
                name,
                "editor" | "run_button" | "stop_button" | "output_tabs" | "output" | "bottom_row"
            );
            if r.right > r.left && r.bottom > r.top && !(hidden && in_bottom) {
                probe::widget(&title, name, r.left, r.top, r.width(), r.height());
            }
        };
        let w = &self.w;
        // Controls sit inside panes of the dividers: lift every rectangle to the window.
        let abs = |id| self.backend.window_bounds(id).unwrap_or_default();
        self.w.strip.get().probe(ui, &title);
        let plain = [
            ("toggle_bottom", w.toggle_bottom.get().id()),
            ("search", w.search.get().id()),
            ("explorer_tabs", w.explorer.get().id()),
            ("doc", w.doc.get().id()),
            ("form_call", w.form_call.get().id()),
            ("form_insert", w.form_insert.get().id()),
            ("form_close", w.form_close.get().id()),
            ("bottom_row", w.bottom.get().id()),
            ("editor", w.editor.get().0.id()),
            ("run_button", w.run.get().id()),
            ("stop_button", w.stop.get().id()),
            ("output_tabs", w.output.get().id()),
        ];
        for (name, id) in plain {
            say(name, abs(id));
        }
        for (name, id) in w.actions.all() {
            say(&format!("btn:{name}"), abs(id));
        }
        self.probe_dividers(&say, &abs, ui.dpi());
        // Where a session presses to grab the page's scroll thumb at the top: the bar's first 90 pixels.
        if let Some(view) = w.doc_scroll.try_get() {
            let r = abs(view.id());
            let bar = 12 * scale;
            say(
                "doc_bar_top",
                Rect::new(r.right - bar, r.top, r.right, (r.top + 90 * scale).min(r.bottom)),
            );
        }
        // The Explorer's pages and the Output pages sit under their tab bars.
        let page = |parent: xui_core::backend::WidgetId, child: xui_core::backend::WidgetId| {
            let p = abs(parent);
            let c = ui.bounds(child);
            Rect::new(
                p.left + c.left,
                p.top + TAB_BAR * scale + c.top,
                p.left + c.right,
                p.top + TAB_BAR * scale + c.bottom,
            )
        };
        let explorer = w.explorer.get().id();
        say("tree", page(explorer, w.tree.get().id()));
        say("live_list", page(explorer, w.live.get().id()));
        say("topics_list", page(explorer, w.topics.get().id()));
        say("gallery_list", page(explorer, w.gallery.get().id()));
        let sel = w.output.get().selected();
        if let Some(run) = self.pages.get(sel) {
            say("output", page(w.output.get().id(), run.list.get().id()));
        }
        // The rows a session clicks: a tree has no header, a list has its column titles.
        let rows_in = |name: &str, rect: Rect, header: i32| {
            self.say_rows(&say, name, rect, header, scale);
        };
        rows_in("tree_row", page(explorer, w.tree.get().id()), 0);
        rows_in("live_row", page(explorer, w.live.get().id()), LIST_HEADER * scale);
        rows_in("topics_row", page(explorer, w.topics.get().id()), LIST_HEADER * scale);
        rows_in("gallery_row", page(explorer, w.gallery.get().id()), LIST_HEADER * scale);
    }

    /// The first rows of a list as `name:0`, `name:1`, ...
    fn say_rows(&self, say: &dyn Fn(&str, Rect), name: &str, list: Rect, header: i32, scale: i32) {
        for k in 0..PROBE_ROWS {
            let top = list.top + header + k * ROW_HEIGHT * scale;
            if top + ROW_HEIGHT * scale > list.bottom {
                break;
            }
            say(
                &format!("{name}:{k}"),
                Rect::new(list.left, top, list.right, top + ROW_HEIGHT * scale),
            );
        }
    }

    /// Close: kill every run, close the Topic monitor (which ends every
    /// subscription), say so, quit.
    pub fn quit(&mut self, ui: &mut Ui<Msg>) {
        self.runs.stop_all();
        self.save_panes();
        self.topics.close();
        trace::pass!(Channel::Playground, "QUIT");
        ui.quit();
    }
}

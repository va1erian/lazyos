//! The report form: a frame over the page (xui's `Dialog` holds text and one entry only, so a form
//! is a frame in the window's layout that is shown and hidden). What happened, what was expected
//! and the steps, then one tick box per part a report may carry, each with a line showing what it
//! would add, and the buttons.
//!
//! It covers the page while it is up, which makes it modal for the page; the toolbar and the menus
//! stay usable. **Send to LazyAgent** is a disabled placeholder where `os.lazy.agent.report.v1` will
//! plug in (docs/lazyweb-plan.md, section 7).

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    button, checkbox, column, group, label, multiline_edit, row, Entry, Handle, LayoutExt,
};
use xui_core::geometry::Rect;
use xui_core::layout::Align;
use xui_core::widget::{Button, CheckBox, GroupBox, HasText, Label, MultilineEdit};

use super::Include;
use crate::app::Msg;

/// What the form raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportMsg {
    /// A tick box changed: the previews follow.
    Toggled,
    CopyMarkdown,
    SaveMarkdown,
    Cancel,
    /// Escape: put the form away and keep what was typed.
    Hide,
}

/// The tick boxes' texts, in the order of [`Include`]'s fields.
const TICKS: [&str; 5] = [
    "Version and tab count",
    "Page host",
    "Page address and path (no query, user)",
    "Window trace, last 200 lines",
    "Engine events this run",
];
/// Which boxes start ticked.
const DEFAULT_ON: [bool; 5] = [true, false, false, false, false];

/// The form's widgets, bound while the window is built.
#[derive(Default)]
pub struct Handles {
    frame: Handle<GroupBox<Msg>>,
    happened: Handle<MultilineEdit<Msg>>,
    expected: Handle<MultilineEdit<Msg>>,
    steps: Handle<MultilineEdit<Msg>>,
    ticks: [Handle<CheckBox<Msg>>; 5],
    previews: [Handle<Label<Msg>>; 5],
    copy: Handle<Button<Msg>>,
    save: Handle<Button<Msg>>,
    send: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
    /// Whether the frame is up (the window's key mapper reads it: Escape hides it).
    open: Rc<Cell<bool>>,
}

impl Handles {
    pub fn open_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.open)
    }
}

fn text_field(title: &str, handle: &Handle<MultilineEdit<Msg>>) -> Entry<Msg> {
    column()
        .gap(2)
        .children((
            label(title).caption().fixed(16),
            multiline_edit().bind(handle).fixed(44),
        ))
        .fixed(62)
}

fn tick_row(index: usize, h: &Handles) -> Entry<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .children((
            checkbox(TICKS[index])
                .checked(DEFAULT_ON[index])
                .on_toggle(|_| Msg::Report(ReportMsg::Toggled))
                .bind(&h.ticks[index])
                .width(260),
            label("").bind(&h.previews[index]).fill(1),
        ))
        .fixed(24)
}

/// The frame for the window's layout, over the page.
pub fn layout(h: &Handles) -> impl LayoutExt<Msg> {
    let content = column().gap(6).padding(10).children((
        text_field("What happened", &h.happened),
        text_field("What you expected", &h.expected),
        text_field("Steps to reproduce", &h.steps),
        tick_row(0, h),
        tick_row(1, h),
        tick_row(2, h),
        tick_row(3, h),
        tick_row(4, h),
        row()
            .gap(8)
            .align(Align::Center)
            .children((
                button("Copy Markdown")
                    .on_click(Msg::Report(ReportMsg::CopyMarkdown))
                    .primary()
                    .bind(&h.copy)
                    .width(130),
                button("Save .md")
                    .on_click(Msg::Report(ReportMsg::SaveMarkdown))
                    .bind(&h.save)
                    .width(100),
                // Where os.lazy.agent.report.v1 plugs in: a click will call `Submit` with the same
                // report. Until LazyAgent exists the button is there and off.
                button("Send to LazyAgent")
                    .tooltip("LazyAgent is not installed")
                    .bind(&h.send)
                    .width(150),
                label("").fill(1),
                button("Cancel")
                    .on_click(Msg::Report(ReportMsg::Cancel))
                    .bind(&h.cancel)
                    .width(90),
            ))
            .fixed(34),
    ));
    group("Report an issue", content).bind(&h.frame)
}

/// The form and its widgets.
pub struct Form {
    frame: Rc<GroupBox<Msg>>,
    happened: Rc<MultilineEdit<Msg>>,
    expected: Rc<MultilineEdit<Msg>>,
    steps: Rc<MultilineEdit<Msg>>,
    ticks: [Rc<CheckBox<Msg>>; 5],
    previews: [Rc<Label<Msg>>; 5],
    copy: Rc<Button<Msg>>,
    save: Rc<Button<Msg>>,
    send: Rc<Button<Msg>>,
    cancel: Rc<Button<Msg>>,
    open: Rc<Cell<bool>>,
}

impl Form {
    pub fn new(h: &Handles) -> Form {
        let form = Form {
            frame: h.frame.get(),
            happened: h.happened.get(),
            expected: h.expected.get(),
            steps: h.steps.get(),
            ticks: std::array::from_fn(|i| h.ticks[i].get()),
            previews: std::array::from_fn(|i| h.previews[i].get()),
            copy: h.copy.get(),
            save: h.save.get(),
            send: h.send.get(),
            cancel: h.cancel.get(),
            open: Rc::clone(&h.open),
        };
        form.send.set_enabled(false);
        form
    }

    pub fn hide(&self, ui: &Ui<Msg>) {
        self.open.set(false);
        ui.set_visible(self.frame.id(), false);
    }

    pub fn is_open(&self, ui: &Ui<Msg>) -> bool {
        ui.is_visible(self.frame.id())
    }

    /// Shows the form (what was typed stays), with `previews` beside the tick boxes.
    pub fn show(&self, ui: &Ui<Msg>, previews: &[String; 5]) {
        self.set_previews(previews);
        self.open.set(true);
        ui.set_visible(self.frame.id(), true);
        ui.relayout();
        self.happened.focus();
    }

    /// Forgets what was typed and the ticks, for the next report.
    pub fn clear(&self) {
        for field in [&self.happened, &self.expected, &self.steps] {
            field.set_text("");
        }
        for (tick, on) in self.ticks.iter().zip(DEFAULT_ON) {
            tick.set_checked(on);
        }
    }

    pub fn set_previews(&self, previews: &[String; 5]) {
        for (label, text) in self.previews.iter().zip(previews) {
            label.set_text(text);
        }
    }

    /// The three texts the person typed.
    pub fn texts(&self) -> [String; 3] {
        [
            self.happened.text(),
            self.expected.text(),
            self.steps.text(),
        ]
    }

    /// The tick boxes as an [`Include`].
    pub fn include(&self) -> Include {
        let on: Vec<bool> = self.ticks.iter().map(|t| t.is_checked()).collect();
        Include {
            build: on[0],
            host: on[1],
            address: on[2],
            trace: on[3],
            events: on[4],
        }
    }

    /// Where the form and its buttons are, by the names a session script clicks.
    pub fn controls(&self, ui: &Ui<Msg>) -> Vec<(&'static str, Rect)> {
        let mut controls = vec![
            ("report_form", ui.bounds(self.frame.id())),
            ("report_happened", ui.bounds(self.happened.id())),
            ("report_expected", ui.bounds(self.expected.id())),
            ("report_steps", ui.bounds(self.steps.id())),
            ("report_copy", ui.bounds(self.copy.id())),
            ("report_save", ui.bounds(self.save.id())),
            ("report_send", ui.bounds(self.send.id())),
            ("report_cancel", ui.bounds(self.cancel.id())),
        ];
        let names = [
            "report_tick_build",
            "report_tick_host",
            "report_tick_address",
            "report_tick_trace",
            "report_tick_events",
        ];
        for (name, tick) in names.into_iter().zip(&self.ticks) {
            controls.push((name, ui.bounds(tick.id())));
        }
        controls
    }
}

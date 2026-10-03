//! The Time & Date page: the current time, setting the clock, the time zone
//! and the taskbar clock format.
//!
//! The clock and the zone go to `timed` ([`System::set_time`] needs
//! `CAP_SYS_TIME`, [`System::set_zone`] persists to confd); the format
//! toggles are confd keys the LazyShell taskbar follows live ([`time_ops`]).

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, CheckBox, Edit, Label, ListView, Panel};
use xui_core::{HasText, Rect};

use crate::app::Msg;
use crate::store::ConfigStore;
use crate::system::System;
use crate::time_ops;

/// Messages the Time & Date page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeMsg {
    /// Set the clock from the date and time fields.
    Apply,
    /// Refill the fields with the current time.
    Now,
    /// A zone row was selected.
    Zone(usize),
    Clock24(bool),
    Seconds(bool),
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    crate::layout::rect(x, y, w, h)
}

fn send(msg: TimeMsg) -> Option<Msg> {
    Some(Msg::Time(msg))
}

/// The page's widgets.
pub struct TimePage {
    panel: Panel<Msg>,
    now: Label<Msg>,
    date: Edit<Msg>,
    time: Edit<Msg>,
    zones: ListView<Msg>,
    clock24: CheckBox<Msg>,
    seconds: CheckBox<Msg>,
    _labels: Vec<Label<Msg>>,
    _buttons: Vec<Button<Msg>>,
}

impl TimePage {
    /// Build the page (hidden state is the caller's job) inside `bounds`.
    pub fn build(ui: &Ui<Msg>, bounds: Rect) -> Result<TimePage> {
        let panel = Panel::new(ui, bounds)?;
        let p = panel.ui();
        let labels = vec![
            Label::new(p, rect(20, 14, 300, 20), "Current time")?,
            Label::new(p, rect(20, 72, 300, 20), "Set the date and local time")?,
            Label::new(p, rect(20, 140, 220, 20), "Time zone")?,
            Label::new(p, rect(260, 140, 210, 20), "Taskbar clock")?,
        ];
        let now = Label::new(p, rect(20, 38, 450, 20), "")?;
        let date = Edit::new(p, rect(20, 96, 120, 26), "")?.cue("YYYY-MM-DD");
        let time = Edit::new(p, rect(148, 96, 96, 26), "")?.cue("HH:MM:SS");
        let buttons = vec![
            Button::new(p, rect(252, 95, 70, 28), "Apply")?.on_click(|| send(TimeMsg::Apply)),
            Button::new(p, rect(328, 95, 70, 28), "Now")?.on_click(|| send(TimeMsg::Now)),
        ];
        let names: Vec<&str> = timezone::ZONES.iter().map(|zone| zone.name).collect();
        let zones = ListView::new(p, rect(20, 164, 220, 200), &names)?
            .multi_select(false)
            .on_select(|i| send(TimeMsg::Zone(i)));
        let clock24 = CheckBox::new(p, rect(260, 164, 210, 24), "24-hour clock")?
            .on_toggle(|on| send(TimeMsg::Clock24(on)));
        let seconds = CheckBox::new(p, rect(260, 194, 210, 24), "Show seconds")?
            .on_toggle(|on| send(TimeMsg::Seconds(on)));
        Ok(TimePage {
            panel,
            now,
            date,
            time,
            zones,
            clock24,
            seconds,
            _labels: labels,
            _buttons: buttons,
        })
    }

    pub fn set_visible(&self, visible: bool) {
        self.panel.set_visible(visible);
    }

    /// Show the current time and zone, prefill the fields, and point the
    /// toggles at the stored format (no events are raised).
    pub fn load(&self, store: &dyn ConfigStore, system: &dyn System) {
        let format = time_ops::clock_format(store);
        self.clock24.set_checked(format.hour24);
        self.seconds.set_checked(format.seconds);
        self.show_now(system);
    }

    /// Refresh the current-time line, the fields and the zone selection.
    fn show_now(&self, system: &dyn System) {
        match system.now() {
            Some(now) => {
                self.now.set_text(&time_ops::summary(&now));
                let (date, time) = time_ops::fields(&now);
                self.date.set_text(&date);
                self.time.set_text(&time);
                self.zones.select(time_ops::zone_index(&now.zone));
            }
            None => self
                .now
                .set_text("The time service is not running: the clock cannot be changed."),
        }
    }

    /// Handle one message; returns the status line text.
    pub fn update(&self, msg: TimeMsg, store: &dyn ConfigStore, system: &dyn System) -> String {
        let outcome = match msg {
            TimeMsg::Now => {
                self.show_now(system);
                return String::new();
            }
            TimeMsg::Apply => {
                // A typo is reported as typed; only a refused write is an error.
                return match self.apply(system) {
                    Ok(text) => {
                        self.show_now(system);
                        text
                    }
                    Err(text) => text,
                };
            }
            TimeMsg::Zone(i) => match timezone::ZONES.get(i) {
                Some(zone) => system
                    .set_zone(zone.name)
                    .map(|()| format!("Time zone set to {}.", zone.name)),
                None => return String::new(),
            },
            TimeMsg::Clock24(on) => time_ops::set_clock24(store, on).map(|()| {
                String::from(if on {
                    "Clock shows 24-hour time."
                } else {
                    "Clock shows 12-hour time."
                })
            }),
            TimeMsg::Seconds(on) => time_ops::set_show_seconds(store, on).map(|()| {
                String::from(if on {
                    "Clock shows seconds."
                } else {
                    "Clock hides seconds."
                })
            }),
        };
        match outcome {
            Ok(text) => {
                if !matches!(msg, TimeMsg::Clock24(_) | TimeMsg::Seconds(_)) {
                    self.show_now(system);
                }
                text
            }
            Err(error) => format!("Could not save: {error}"),
        }
    }

    /// Set the clock from the fields, read in the zone currently in effect.
    fn apply(&self, system: &dyn System) -> std::result::Result<String, String> {
        let zone = system
            .now()
            .and_then(|now| timezone::find(&now.zone))
            .unwrap_or_else(timezone::default_zone);
        let unix = time_ops::typed_instant(zone, &self.date.text(), &self.time.text())?;
        system
            .set_time(unix)
            .map_err(|error| format!("Could not set the time: {error}"))?;
        Ok(String::from("Date and time set."))
    }
}

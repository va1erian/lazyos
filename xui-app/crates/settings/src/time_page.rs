//! The Time & Date page: the current time, setting the clock, the time zone
//! and the taskbar clock format.
//!
//! The clock and the zone go to `timed` ([`System::set_time`] needs
//! `CAP_SYS_TIME`, [`System::set_zone`] persists to confd); the format
//! toggles are confd keys the LazyShell taskbar follows live ([`time_ops`]).

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, checkbox, column, edit, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{CheckBox, Edit, Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
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

/// The page's widgets.
pub struct TimePage {
    now: Rc<Label<Msg>>,
    date: Rc<Edit<Msg>>,
    time: Rc<Edit<Msg>>,
    zones: Rc<ListView<Msg>>,
    clock24: Rc<CheckBox<Msg>>,
    seconds: Rc<CheckBox<Msg>>,
    _mounted: Mounted<Msg>,
}

impl TimePage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<TimePage> {
        let (now, date, time, zones) = (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let (clock24, seconds) = (Handle::new(), Handle::new());
        let zone_names: Vec<&str> = timezone::ZONES.iter().map(|zone| zone.name).collect();
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(16).children((
                column()
                    .gap(4)
                    .children((label("Current time"), label("").bind(&now))),
                column().gap(6).children((
                    label("Set the date and local time"),
                    row().gap(8).children((
                        edit().placeholder("YYYY-MM-DD").bind(&date).width(120),
                        edit().placeholder("HH:MM:SS").bind(&time).width(96),
                        button("Apply").on_click(Msg::Time(TimeMsg::Apply)),
                        button("Now").on_click(Msg::Time(TimeMsg::Now)),
                    )),
                )),
                row()
                    .gap(20)
                    .children((
                        column()
                            .gap(8)
                            .children((
                                label("Time zone"),
                                choice_list(&zone_names)
                                    .on_select(|i| Msg::Time(TimeMsg::Zone(i)))
                                    .bind(&zones)
                                    .fill(1),
                            ))
                            .fill(1),
                        column()
                            .gap(8)
                            .children((
                                label("Taskbar clock"),
                                checkbox("24-hour clock")
                                    .on_toggle(|on| Msg::Time(TimeMsg::Clock24(on)))
                                    .bind(&clock24),
                                checkbox("Show seconds")
                                    .on_toggle(|on| Msg::Time(TimeMsg::Seconds(on)))
                                    .bind(&seconds),
                            ))
                            .fill(1),
                    ))
                    .fill(1),
            )),
        )?;
        Ok(TimePage {
            now: now.get(),
            date: date.get(),
            time: time.get(),
            zones: zones.get(),
            clock24: clock24.get(),
            seconds: seconds.get(),
            _mounted: mounted,
        })
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

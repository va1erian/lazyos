//! The Time & Date page: the current time, setting the clock, the time zone
//! and the taskbar clock format.
//!
//! All four are machine settings, and all four ask an administrator
//! (docs/accounts-plan.md U2): the clock and the zone go to `timed`
//! ([`System::set_time`], [`System::set_zone`], through `elevd` for a
//! session), the format toggles are `sys/time/*` confd keys the LazyShell
//! taskbar follows live ([`time_ops`]), written through `elevd` too. So each
//! writes on an explicit action, once: moving through the zone list only
//! chooses, **Use this zone** applies it; a refused or cancelled change shows
//! the stored value again.
//!
//! Automatic time (docs/ntp-plan.md) is two more `sys/time/ntp/*` keys that
//! `timed` follows live: a switch, and the server list that **Use this
//! server** applies; both sit under the taskbar clock toggles, so the layout
//! the screenshot sessions click through is unchanged. **Sync now** is open
//! to everyone (`timed` rate-limits it). While the switch is on, the hand-set
//! clock is refused with a reason, not hidden.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, checkbox, column, edit, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{CheckBox, Edit, Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::store::ConfigStore;
use crate::system::{NtpState, System};
use crate::time_ops;

/// Messages the Time & Date page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeMsg {
    /// Set the clock from the date and time fields.
    Apply,
    /// Refill the fields with the current time.
    Now,
    /// A zone row was selected (nothing is written).
    Zone(usize),
    /// Switch to the selected zone.
    ApplyZone,
    Clock24(bool),
    Seconds(bool),
    /// The "set the time automatically" switch.
    Auto(bool),
    /// Store the server list from its field.
    ApplyServers,
    /// Ask a time server now.
    SyncNow,
}

/// The page's widgets.
pub struct TimePage {
    now: Rc<Label<Msg>>,
    date: Rc<Edit<Msg>>,
    time: Rc<Edit<Msg>>,
    zones: Rc<ListView<Msg>>,
    clock24: Rc<CheckBox<Msg>>,
    seconds: Rc<CheckBox<Msg>>,
    auto: Rc<CheckBox<Msg>>,
    auto_status: Rc<Label<Msg>>,
    servers: Rc<Edit<Msg>>,
    _mounted: Mounted<Msg>,
}

impl TimePage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<TimePage> {
        let (now, date, time, zones) = (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let (clock24, seconds) = (Handle::new(), Handle::new());
        let (auto, auto_status, servers) = (Handle::new(), Handle::new(), Handle::new());
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
                                row().child(
                                    button("Use this zone").on_click(Msg::Time(TimeMsg::ApplyZone)),
                                ),
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
                                label("Network time"),
                                checkbox("Set the time automatically")
                                    .on_toggle(|on| Msg::Time(TimeMsg::Auto(on)))
                                    .bind(&auto),
                                label("").bind(&auto_status),
                                edit().placeholder("pool.ntp.org").bind(&servers).width(280),
                                row().gap(8).children((
                                    button("Use this server")
                                        .on_click(Msg::Time(TimeMsg::ApplyServers)),
                                    button("Sync now").on_click(Msg::Time(TimeMsg::SyncNow)),
                                )),
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
            auto: auto.get(),
            auto_status: auto_status.get(),
            servers: servers.get(),
            _mounted: mounted,
        })
    }

    /// Show the current time and zone, prefill the fields, and point the
    /// toggles at the stored format (no events are raised).
    pub fn load(&self, store: &dyn ConfigStore, system: &dyn System) {
        let format = time_ops::clock_format(store);
        self.clock24.set_checked(format.hour24);
        self.seconds.set_checked(format.seconds);
        self.auto.set_checked(time_ops::ntp_enabled(store));
        self.servers.set_text(&time_ops::ntp_servers(store));
        self.show_now(store, system);
    }

    /// Refresh the current-time line, the fields and the zone selection.
    fn show_now(&self, store: &dyn ConfigStore, system: &dyn System) {
        let now = system.now();
        let unix = now.as_ref().map_or(0, |now| now.unix);
        self.auto_status.set_text(&time_ops::ntp_summary(
            time_ops::ntp_enabled(store),
            system.ntp_status().as_ref(),
            unix,
        ));
        match now {
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
                self.show_now(store, system);
                return String::new();
            }
            TimeMsg::Apply => {
                // Only while a time client could put it back: switched on and,
                // as `timed` reports, with a network (an image without one
                // keeps its hand-set clock, whatever the default says).
                let acting = time_ops::ntp_enabled(store)
                    && system.ntp_status().is_some_and(|status| {
                        !matches!(
                            status.state,
                            NtpState::Disabled | NtpState::WaitingForNetwork
                        )
                    });
                if acting {
                    return String::from(
                        "The clock is set automatically. Turn that off to set it by hand.",
                    );
                }
                // A typo is reported as typed; only a refused write is an error.
                return match self.apply(system) {
                    Ok(text) => {
                        self.show_now(store, system);
                        text
                    }
                    Err(text) => text,
                };
            }
            TimeMsg::ApplyServers => {
                return match time_ops::set_ntp_servers(store, &self.servers.text()) {
                    Ok(()) => {
                        self.servers.set_text(&time_ops::ntp_servers(store));
                        String::from("Time servers saved.")
                    }
                    // A typo is reported as typed; the stored list stays.
                    Err(text) => text,
                };
            }
            TimeMsg::SyncNow => {
                let text = match system.ntp_sync_now() {
                    Ok(()) => String::from("Asking a time server..."),
                    Err(text) => text,
                };
                self.show_now(store, system);
                return text;
            }
            TimeMsg::Zone(_) => return String::new(),
            TimeMsg::ApplyZone => {
                match self.zones.selected().and_then(|i| timezone::ZONES.get(i)) {
                    Some(zone) if system.now().is_some_and(|now| now.zone == zone.name) => {
                        return format!("The time zone already is {}.", zone.name);
                    }
                    Some(zone) => system
                        .set_zone(zone.name)
                        .map(|()| format!("Time zone set to {}.", zone.name)),
                    None => return String::from("Select a time zone first."),
                }
            }
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
            TimeMsg::Auto(on) => time_ops::set_ntp_enabled(store, on).map(|()| {
                String::from(if on {
                    "The clock is set automatically when there is a network."
                } else {
                    "The clock is set by hand."
                })
            }),
        };
        match outcome {
            Ok(text) => {
                if let TimeMsg::Auto(on) = msg {
                    // Already so when the click raised it; not when a message
                    // came from anywhere else.
                    self.auto.set_checked(on);
                }
                if !matches!(msg, TimeMsg::Clock24(_) | TimeMsg::Seconds(_)) {
                    self.show_now(store, system);
                }
                text
            }
            Err(error) => {
                // Show what is in effect, not what was refused.
                self.load(store, system);
                format!("Could not save: {error}")
            }
        }
    }

    /// Set the clock from the fields, read in the zone currently in effect.
    fn apply(&self, system: &dyn System) -> std::result::Result<String, String> {
        let zone = system
            .now()
            .and_then(|now| timezone::find(&now.zone))
            .unwrap_or_else(timezone::default_zone);
        let unix = time_ops::typed_instant(zone, &self.date.text(), &self.time.text())?;
        if unix < sntp::CLOCK_FLOOR_UNIX {
            // `timed` would refuse it too; say why, in the user's terms.
            let floor = time_ops::fields(&crate::system::Now {
                unix: sntp::CLOCK_FLOOR_UNIX,
                offset: 0,
                zone: String::from("UTC"),
            });
            return Err(format!(
                "The clock cannot be set earlier than {} (UTC).",
                floor.0
            ));
        }
        system
            .set_time(unix)
            .map_err(|error| format!("Could not set the time: {error}"))?;
        Ok(String::from("Date and time set."))
    }

    /// The machine change `msg` names, checked against what is in effect,
    /// ready for [`run`] on a worker thread; `None` leaves the message to
    /// [`TimePage::update`] (it writes nothing, or its friendly refusal is
    /// a read only).
    pub fn change(
        &self,
        msg: &TimeMsg,
        store: &dyn ConfigStore,
        system: &dyn System,
    ) -> Option<Change> {
        match msg {
            TimeMsg::Clock24(on) => Some(Change::Clock24(*on)),
            TimeMsg::Seconds(on) => Some(Change::Seconds(*on)),
            TimeMsg::Auto(on) => Some(Change::Auto(*on)),
            TimeMsg::ApplyServers => Some(Change::Servers(self.servers.text())),
            TimeMsg::ApplyZone => {
                let zone = self.zones.selected().and_then(|i| timezone::ZONES.get(i))?;
                // "Already this zone" costs a read, which asks nobody.
                if system.now().is_some_and(|now| now.zone == zone.name) {
                    return None;
                }
                Some(Change::Zone(zone.name.to_owned()))
            }
            TimeMsg::Apply => {
                // Only while a time client could put it back: switched on
                // and networked, as in the page's own answer.
                let acting = time_ops::ntp_enabled(store)
                    && system.ntp_status().is_some_and(|status| {
                        !matches!(
                            status.state,
                            NtpState::Disabled | NtpState::WaitingForNetwork
                        )
                    });
                if acting {
                    return None;
                }
                // A typo is answered by the page; a well-formed request
                // waits for the administrator on the worker.
                let zone = system
                    .now()
                    .and_then(|now| timezone::find(&now.zone))
                    .unwrap_or_else(timezone::default_zone);
                time_ops::typed_instant(zone, &self.date.text(), &self.time.text())
                    .is_ok()
                    .then(|| Change::Clock(self.date.text(), self.time.text()))
            }
            TimeMsg::Now | TimeMsg::Zone(_) | TimeMsg::SyncNow => None,
        }
    }
}

/// A machine change the Time page raised, captured on the UI thread and run
/// on the worker thread ([`run`]): every one of them asks an administrator
/// at the prompt, which the window must not spend standing still (issue
/// #720). The friendly guards (already this zone, the clock is set
/// automatically, a typo) are checked on the calling thread first, so a
/// change that would only report is never taken.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Clock24(bool),
    Seconds(bool),
    /// Switch automatic time.
    Auto(bool),
    /// Store the server list as typed in its field.
    Servers(String),
    /// Switch to this zone name.
    Zone(String),
    /// Set the clock from the typed date and local time.
    Clock(String, String),
}

/// Run a captured change against a worker thread's store and connection,
/// and say what it left in effect. Takes the *machine* store: the page's
/// `sys/**` writes go through `elevd` there.
pub fn run(
    change: Change,
    store: &dyn ConfigStore,
    system: &dyn System,
) -> std::result::Result<String, String> {
    match change {
        Change::Clock24(on) => time_ops::set_clock24(store, on).map(|_| {
            String::from(if on {
                "Clock shows 24-hour time."
            } else {
                "Clock shows 12-hour time."
            })
        }),
        Change::Seconds(on) => time_ops::set_show_seconds(store, on).map(|_| {
            String::from(if on {
                "Clock shows seconds."
            } else {
                "Clock hides seconds."
            })
        }),
        Change::Auto(on) => time_ops::set_ntp_enabled(store, on).map(|_| {
            String::from(if on {
                "The clock is set automatically when there is a network."
            } else {
                "The clock is set by hand."
            })
        }),
        Change::Servers(text) => {
            time_ops::set_ntp_servers(store, &text).map(|_| String::from("Time servers saved."))
        }
        Change::Zone(zone) => system
            .set_zone(&zone)
            .map(|()| format!("Time zone set to {zone}.")),
        // The zone is read once more: the prompt may have taken long enough
        // for it to change, and `timed` answers in its own zone.
        Change::Clock(date, time) => {
            let zone = system
                .now()
                .and_then(|now| timezone::find(&now.zone))
                .unwrap_or_else(timezone::default_zone);
            let unix = time_ops::typed_instant(zone, &date, &time)?;
            if unix < sntp::CLOCK_FLOOR_UNIX {
                // `timed` would refuse it too; say why, in the user's terms.
                let floor = time_ops::fields(&crate::system::Now {
                    unix: sntp::CLOCK_FLOOR_UNIX,
                    offset: 0,
                    zone: String::from("UTC"),
                });
                return Err(format!(
                    "The clock cannot be set earlier than {} (UTC).",
                    floor.0
                ));
            }
            system
                .set_time(unix)
                .map(|()| String::from("Date and time set."))
                .map_err(|error| format!("Could not set the time: {error}"))
        }
    }
}

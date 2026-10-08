//! The `fabricmon` window's widgets: the layout, and how each refresh fills it.
//!
//! Everything is a standard xui widget placed by a layout: the fabric counters
//! are a key/value grid in a group box, the per-task usage, registry and
//! topics tables are list views, and the footer is a status bar. Resizing,
//! HiDPI and the theme come from the layout and the widgets.

use xui_app::fabric::{errno_text, FabricStats, Topics};
use xui_app::format::{bytes, hex_id};
use xui_core::app::Ui;
use xui_core::prelude::*;

use crate::{Msg, State};

/// The keys the header reminds of.
const KEYS: &str =
    "syscall 5 · registry list · topics broker · [r] refresh  [f] fabric  [n] names  [c] compact  [q] quit";

/// The counters, in reading order; [`counters`] gives their values.
const COUNTERS: [&str; 22] = [
    "channels",
    "endpoints",
    "queued messages",
    "queued bytes",
    "outstanding txns",
    "calls",
    "replies",
    "one-way messages",
    "timeouts",
    "cancels",
    "drops",
    "handles",
    "shared buffers",
    "buffer bytes",
    "buffer mappings",
    "zero-copy handoffs",
    "kernel services",
    "ACL rules",
    "ACL loaded",
    "audit denies",
    "audit allows",
    "audit events",
];

/// How many counters each of the three counter columns holds.
const PER_COLUMN: usize = COUNTERS.len().div_ceil(3);

/// The compact view's headline counters.
const HEADLINES: [&str; 4] = ["channels", "endpoints", "queued messages", "registry names"];

/// One `key  value` line of the compact view.
#[derive(Default)]
struct Headline {
    key: Handle<Label<Msg>>,
    value: Handle<Label<Msg>>,
}

/// Every widget the app changes after start-up.
#[derive(Default)]
pub(crate) struct Widgets {
    hint: Handle<Label<Msg>>,
    pub(crate) tabs: Handle<Tabs<Msg>>,
    counters: [Handle<Label<Msg>>; COUNTERS.len()],
    tasks_heading: Handle<Label<Msg>>,
    tasks: Handle<ListView<Msg>>,
    registry_heading: Handle<Label<Msg>>,
    registry: Handle<ListView<Msg>>,
    topics_heading: Handle<Label<Msg>>,
    topics_note: Handle<Label<Msg>>,
    topics: Handle<ListView<Msg>>,
    headlines: [Headline; 4],
    status: Handle<StatusBar<Msg>>,
}

impl Widgets {
    /// The whole window: a header, the tabs (or, when compact, the headline
    /// counters) and the status bar.
    pub(crate) fn layout(&self) -> Layout<Msg> {
        let mut headlines = Vec::new();
        for (line, key) in self.headlines.iter().zip(HEADLINES) {
            headlines.push(label(key).bind(&line.key).into_entry());
            headlines.push(label("").bind(&line.value).into_entry());
        }
        column().padding(12).gap(10).children((
            row()
                .gap(16)
                .align(Align::Center)
                .children((label("fabricmon").title(), label(KEYS).bind(&self.hint))),
            tabs()
                .page("Fabric", self.fabric_page())
                .page("Names & topics", self.names_page())
                .bind(&self.tabs)
                .fill(1),
            grid([Track::Auto, Track::Fill(1)])
                .gap(6)
                .children(headlines),
            status_bar(&["", "", "", ""]).bind(&self.status),
        ))
    }

    fn fabric_page(&self) -> Layout<Msg> {
        let mut columns = Vec::new();
        for (keys, values) in COUNTERS
            .chunks(PER_COLUMN)
            .zip(self.counters.chunks(PER_COLUMN))
        {
            let mut cells = Vec::new();
            for (key, value) in keys.iter().zip(values) {
                cells.push(label(*key).into_entry());
                cells.push(label("").bind(value).into_entry());
            }
            columns.push(
                grid([Track::Auto, Track::Fill(1)])
                    .gap(6)
                    .children(cells)
                    .fill(1),
            );
        }
        column().padding(8).gap(10).children((
            group("Fabric — stats ABI v4", row().gap(24).children(columns)),
            label("Per-task usage").bind(&self.tasks_heading),
            list()
                .column_right("slot", 60)
                .column_right("handles", 90)
                .column_right("buffers", 90)
                .column_right("buffer bytes", 120)
                .bind(&self.tasks)
                .fill(1),
        ))
    }

    fn names_page(&self) -> Layout<Msg> {
        column().padding(8).gap(8).children((
            label("Registry").bind(&self.registry_heading),
            list()
                .column("name", 260)
                .column("owner", 90)
                .column("interfaces", Fill)
                .bind(&self.registry)
                .fill(2),
            label("Topics — broker").bind(&self.topics_heading),
            label("").bind(&self.topics_note),
            list()
                .column("topic", 260)
                .column_right("subscribers", 100)
                .column("retained", Fill)
                .bind(&self.topics)
                .fill(1),
        ))
    }

    /// Shows the full view, or only the header title and the headlines.
    pub(crate) fn set_compact(&self, ui: &Ui<Msg>, compact: bool) {
        for line in &self.headlines {
            ui.set_visible(line.key.get().id(), compact);
            ui.set_visible(line.value.get().id(), compact);
        }
        for id in [
            self.hint.get().id(),
            self.tabs.get().id(),
            self.status.get().id(),
        ] {
            ui.set_visible(id, !compact);
        }
    }

    /// Fills every widget from `state` after `refreshes` refreshes.
    pub(crate) fn show(&self, state: &State, refreshes: u64) {
        match &state.stats {
            Some(stats) => self.show_stats(stats),
            None => {
                let code = state.stats_error.unwrap_or(-22);
                self.tasks_heading
                    .get()
                    .set_text(&format!("stats syscall failed: {}", errno_text(code)));
                self.headlines[0]
                    .value
                    .get()
                    .set_text(&format!("unavailable (errno {code})"));
            }
        }
        self.headlines[3]
            .value
            .get()
            .set_text(&state.names().to_string());
        self.show_registry(state);
        self.show_topics(state.topics.as_ref());
        self.show_status(state, refreshes);
    }

    fn show_stats(&self, stats: &FabricStats) {
        for (label, value) in self.counters.iter().zip(counters(stats)) {
            label.get().set_text(&value);
        }
        for (line, value) in
            self.headlines
                .iter()
                .zip([stats.channels, stats.endpoints, stats.queued])
        {
            line.value.get().set_text(&value.to_string());
        }
        let rows: Vec<Vec<String>> = stats
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, usage)| usage.live != 0)
            .map(|(slot, usage)| {
                vec![
                    slot.to_string(),
                    usage.handles.to_string(),
                    usage.buffers.to_string(),
                    bytes(usage.buffer_bytes),
                ]
            })
            .collect();
        self.tasks_heading
            .get()
            .set_text(&format!("Per-task usage — {} live slot(s)", rows.len()));
        self.tasks.get().refresh_model(rows);
    }

    fn show_registry(&self, state: &State) {
        let heading = match (&state.registry, state.registry_error) {
            (None, code) => format!("Registry unavailable: {}", errno_text(code.unwrap_or(-22))),
            (Some(_), Some(code)) => format!(
                "Registry — {} name(s) · last read failed: {}",
                state.names(),
                errno_text(code)
            ),
            (Some(_), None) => format!("Registry — {} name(s)", state.names()),
        };
        self.registry_heading.get().set_text(&heading);
        let Some(entries) = &state.registry else {
            return;
        };
        let rows: Vec<Vec<String>> = entries
            .iter()
            .map(|entry| {
                let interfaces = if entry.interfaces.is_empty() {
                    "—".to_string()
                } else {
                    let ids: Vec<String> = entry.interfaces.iter().map(|id| hex_id(*id)).collect();
                    ids.join(", ")
                };
                vec![
                    entry.name.clone(),
                    format!("slot {}", entry.owner_slot),
                    interfaces,
                ]
            })
            .collect();
        self.registry.get().refresh_model(rows);
    }

    fn show_topics(&self, topics: Option<&Topics>) {
        let (heading, note, rows) = match topics {
            Some(Topics::Online(topics)) => {
                let subscriptions: u64 = topics.iter().map(|t| t.subscribers).sum();
                let note = if topics.is_empty() {
                    "no topic has been published since the broker started"
                } else {
                    ""
                };
                let rows = topics
                    .iter()
                    .map(|topic| {
                        vec![
                            topic.topic.clone(),
                            topic.subscribers.to_string(),
                            if topic.retained { "retained" } else { "—" }.to_string(),
                        ]
                    })
                    .collect();
                (
                    format!(
                        "Topics — broker: {} topic(s) seen; {subscriptions} subscription(s)",
                        topics.len()
                    ),
                    note,
                    rows,
                )
            }
            Some(Topics::Offline(code)) => (
                format!("Topics — broker offline: {}", errno_text(*code)),
                "boot the image with a broker (LAZYOS_SERVICES=1 or LAZYOS_MESSENGERD=1) to list topics",
                Vec::new(),
            ),
            None => return,
        };
        self.topics_heading.get().set_text(&heading);
        self.topics_note.get().set_text(note);
        self.topics.get().refresh_model(rows);
    }

    /// The status bar: tick, refreshes, headline counts and the source (or the
    /// last refresh's error).
    fn show_status(&self, state: &State, refreshes: u64) {
        let source = match state.stats_error {
            Some(code) => format!("last refresh failed: errno {code}"),
            None => "stats ABI v4 via syscall 5".to_string(),
        };
        let status = self.status.get();
        status.set_text(0, &format!("tick {}", xui_app::sys::clock_ticks()));
        status.set_text(1, &format!("{refreshes} refresh(es)"));
        status.set_text(
            2,
            &format!(
                "{} name(s) · {} topic(s)",
                state.names(),
                state.topic_count()
            ),
        );
        status.set_text(3, &source);
    }
}

/// The value of each of [`COUNTERS`], in order.
fn counters(s: &FabricStats) -> [String; COUNTERS.len()] {
    let yes_no = |value: u64| if value != 0 { "yes" } else { "no" };
    [
        s.channels.to_string(),
        s.endpoints.to_string(),
        s.queued.to_string(),
        bytes(s.queued_bytes),
        s.outstanding.to_string(),
        s.calls.to_string(),
        s.replies.to_string(),
        s.one_way.to_string(),
        s.timeouts.to_string(),
        s.cancels.to_string(),
        s.drops.to_string(),
        s.handles.to_string(),
        s.buffers.to_string(),
        bytes(s.buffer_bytes),
        s.buffer_mappings.to_string(),
        s.handoffs.to_string(),
        s.services.to_string(),
        s.acl_rules.to_string(),
        yes_no(s.acl_loaded).to_string(),
        s.audit_denies.to_string(),
        s.audit_allows.to_string(),
        s.audit_total.to_string(),
    ]
}

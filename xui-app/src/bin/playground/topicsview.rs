//! What the Topic monitor shows of its data, as pure functions: a tab's events
//! table, the counters line, the selected event's details, a row.

use xui_core::arrange::{column, list, Handle, Layout, LayoutExt};
use xui_core::Fill;
use xui_playground::listener::Event;

use crate::tmsg::TMsg;

/// One tab's page: the decoded events as a table.
pub fn event_page(handle: &Handle<xui_core::widget::ListView<TMsg>>) -> Layout<TMsg> {
    column().child(
        list()
            .column("time", 52)
            .column("topic", 120)
            .column("from", 90)
            .column("#", 36)
            .column("value", Fill)
            .on_select(TMsg::SubRow)
            .bind(handle)
            .fill(1),
    )
}

/// The stats strip: QoS and depth, the broker's counters, what the viewer
/// skipped, and the list's own counts.
pub fn stats_text(s: &xui_playground::listener::StatsStrip) -> String {
    format!(
        "{}/{} q{} d{} m{} drop{} skip{}+{} | {}/{} pin{}{}",
        s.qos, s.depth, s.queued, s.delivered, s.matched, s.drops,
        s.dropped_by_viewer, s.missed_while_paused, s.shown, s.stored, s.pinned,
        if s.paused { " PAUSED" } else { "" }
    )
}

/// The details list's rows for one event: where it came from, then the value tree.
pub fn details_rows(e: &Event) -> Vec<Vec<String>> {
    let mut rows = vec![
        vec![e.topic.clone()],
        vec![format!("from {}", e.publisher_text())],
        vec![format!(
            "sequence {}{}",
            e.sequence,
            if e.retained { " (retained replay)" } else { "" }
        )],
        vec![format!("payload {}", e.body.kind_text())],
        vec![String::new()],
    ];
    rows.extend(e.tree().lines().map(|line| vec![line.to_string()]));
    rows
}

pub fn short_filter(filter: &str) -> String {
    let t: String = filter.chars().take(18).collect();
    if filter.chars().count() > 18 {
        format!("{t}...")
    } else {
        t
    }
}

pub fn row_of(e: &Event) -> Vec<String> {
    let seconds = e.at_ms as f64 / 1000.0;
    vec![
        format!("{seconds:.1}s"),
        e.topic.clone(),
        e.publisher_text(),
        format!("{}{}", e.sequence, if e.retained { "R" } else { "" }),
        e.summary(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_playground::listener::{Config, Listener, RawEvent, Registry};

    /// One stored event, pulled at 2.5 s from slot 7 (no registry: the sender is `?`).
    fn event(retained: bool) -> Event {
        let mut listener = Listener::offline(Config::new("demo/#"));
        let raw = RawEvent {
            topic: "demo/pg/x".to_string(),
            publisher: 7,
            sequence: 3,
            retained,
            payload: vec![1, 2, 3],
        };
        listener.push(2500, raw, &Registry::default());
        let stored = listener.visible().next().cloned();
        stored.expect("the event was stored")
    }

    #[test]
    fn a_long_filter_is_cut_for_its_tab() {
        assert_eq!(short_filter("time/tick"), "time/tick");
        assert_eq!(short_filter("system/stats/memory/and/more"), "system/stats/memor...");
    }

    #[test]
    fn the_details_list_names_the_topic_the_sender_and_the_sequence() {
        let rows = details_rows(&event(true));
        assert_eq!(rows[0], vec!["demo/pg/x".to_string()]);
        assert_eq!(rows[1][0], "from ?", "no registry read names nobody");
        assert_eq!(rows[2][0], "sequence 3 (retained replay)");
        assert!(rows[3][0].starts_with("payload "));
        assert_eq!(rows[4][0], "", "a gap before the value");
        assert!(rows.len() > 5, "the value tree follows");
        assert_eq!(details_rows(&event(false))[2][0], "sequence 3");
    }

    #[test]
    fn a_row_shows_the_time_the_sender_and_a_retained_replay() {
        let row = row_of(&event(true));
        assert_eq!(row[0], "2.5s");
        assert_eq!(row[1], "demo/pg/x");
        assert_eq!(row[3], "3R");
        assert_eq!(row_of(&event(false))[3], "3");
    }

    #[test]
    fn the_strip_text_says_when_paused() {
        let s = xui_playground::listener::StatsStrip {
            qos: "Buffered",
            depth: 64,
            queued: 0,
            delivered: 5,
            matched: 5,
            drops: 0,
            dropped_by_viewer: 0,
            missed_while_paused: 2,
            shown: 3,
            stored: 5,
            pinned: 1,
            paused: true,
        };
        assert_eq!(stats_text(&s), "Buffered/64 q0 d5 m5 drop0 skip0+2 | 3/5 pin1 PAUSED");
    }
}

//! The Publish form: the topic and value typed in the Topic monitor become a
//! one-line script, run once by the main window (`docs/messenger-playground-plan.md`
//! D3: one execution path, so the run has an Output tab and a `RUN` marker like
//! any other).
//!
//! The form lives in [`crate::topicswin`]; the main window fills it from the
//! selected topic ([`Playground::publish_fields`]) and runs what it sends back
//! ([`Playground::publish_request`]).

use xui_core::app::Ui;
use xui_core::widget::HasText;
use xui_playground::snippet::TopicAction;
use xui_playground::util::rhai_str;

use crate::app::{Playground, Sel};
use crate::tmsg::TMsg;
use crate::topicswin::TopicsTool;

impl Playground {
    /// The topic and starting value the Publish form takes from the selection.
    pub fn publish_fields(&self) -> Option<(String, String)> {
        match &self.sel {
            Sel::LiveTopic(name) => Some((name.clone(), "\"hello\"".to_string())),
            Sel::Node(id) => match self.ex.catalog.get(id).map(|n| n.item) {
                Some(xui_playground::catalog::Item::Topic(i, t)) => {
                    let s = xui_playground::snippet::topic(i, t, TopicAction::Publish);
                    Some((
                        xui_playground::rhai_names::concrete_topic(t.pattern),
                        payload_of_snippet(&s.text),
                    ))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The monitor's Publish: run the one-line script, once.
    pub fn publish_request(&mut self, topic: &str, payload: &str, retained: bool) {
        let script = publish_script(topic, payload, retained);
        self.start_run(&script, 1, "publish");
    }
}

impl TopicsTool {
    /// Fill the form (the main window's Publish button) and focus the value.
    pub fn fill_publish(&mut self, topic: &str, payload: &str) {
        self.w.pub_topic.get().set_text(topic);
        self.w.pub_payload.get().set_text(payload);
        self.publish.topic = topic.to_string();
        self.publish.payload = payload.to_string();
        self.w.pub_payload.get().focus();
        self.note("Edit the value and press Publish.");
    }

    /// Listen to the topic typed in the form.
    pub fn publish_listen(&mut self, ui: &mut Ui<TMsg>) {
        let topic = self.publish.topic.trim().to_string();
        if topic.is_empty() {
            self.note("Type a topic (a filter: + one level, # the rest) to listen to.");
            return;
        }
        self.listen(ui, &topic);
    }

    /// Publish: ask the main window to run the form as a one-line script.
    pub fn publish_go(&mut self) {
        let topic = self.publish.topic.trim().to_string();
        if topic.is_empty() {
            self.note("Type a topic to publish to.");
            return;
        }
        self.note("Publishing: the main window runs it (see its Output).");
        self.tell_main(crate::msg::Msg::PublishRequest {
            topic,
            payload: self.publish.payload.clone(),
            retained: self.publish.retained,
        });
    }
}

/// The payload part of `msg::publish("topic", <payload>);`.
fn payload_of_snippet(text: &str) -> String {
    text.split_once(", ")
        .map(|(_, rest)| rest.trim_end().trim_end_matches(';').trim_end_matches(')'))
        .unwrap_or("\"\"")
        .to_string()
}

/// The one-line script a Publish runs.
pub fn publish_script(topic: &str, payload: &str, retained: bool) -> String {
    format!(
        "msg::publish({}, {}, {retained});\nprint(\"published\");\n",
        rhai_str(topic.trim()),
        payload_expression(payload)
    )
}

/// The payload box as a Rhai expression: what looks like one is used as it
/// is, anything else is sent as a string.
fn payload_expression(text: &str) -> String {
    let t = text.trim();
    let looks_like_rhai = t.starts_with('"')
        || t.starts_with("#{")
        || t.starts_with('[')
        || t.starts_with('`')
        || t == "true"
        || t == "false"
        || t == "()"
        || (!t.is_empty() && t.parse::<f64>().is_ok());
    if looks_like_rhai {
        t.to_string()
    } else {
        rhai_str(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_that_is_not_rhai_is_sent_as_a_string() {
        assert_eq!(payload_expression("hello"), "\"hello\"");
        assert_eq!(payload_expression("\"hi\""), "\"hi\"");
        assert_eq!(payload_expression("#{ a: 1 }"), "#{ a: 1 }");
        assert_eq!(payload_expression("42"), "42");
        assert_eq!(payload_expression(""), "\"\"");
    }

    #[test]
    fn the_script_is_one_publish_call_with_the_retained_flag() {
        assert_eq!(
            publish_script(" demo/pg/x ", "hello from publish", false),
            "msg::publish(\"demo/pg/x\", \"hello from publish\", false);\nprint(\"published\");\n"
        );
        assert!(publish_script("a/b", "#{ n: 1 }", true).contains("#{ n: 1 }, true);"));
    }

    #[test]
    fn the_starting_value_is_the_second_argument_of_the_snippet() {
        assert_eq!(
            payload_of_snippet("msg::publish(\"a/b\", #{ n: 1 });\n"),
            "#{ n: 1 }"
        );
        assert_eq!(payload_of_snippet("nothing"), "\"\"");
    }
}

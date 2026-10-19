//! The core's events, and the tick that drains them.

use lazyagent_core::Event;

use super::*;
use crate::rows::Redo;

impl LazyAgentApp {
    pub(super) fn tick(&mut self, ui: &mut Ui<Msg>) {
        let drained = drain::drain(&self.events, drain::BUDGET);
        for event in drained.events {
            self.on_event(ui, event);
        }
        self.apply_layouts();
        if self.rows_dirty {
            self.rebuild_rows();
        }
        self.maintain_layouts();
        let following = self
            .chat
            .view
            .state
            .try_borrow()
            .map(|s| s.transcript.following());
        if let Ok(following) = following {
            if following != self.reported_following {
                self.reported_following = following;
                self.say(Report::Following(following));
            }
        }
        let pending = self.layouts.pending();
        let retry = self.rows_dirty || !self.carried.is_empty();
        let interval = drain::interval(self.busy, pending, drained.cut_short, retry);
        self.set_interval(ui, interval);
    }

    fn on_event(&mut self, ui: &mut Ui<Msg>, event: Event) {
        match event {
            Event::Settings(settings) => {
                let first = !self.settings_known;
                self.settings_known = true;
                self.settings = settings;
                self.apply_theme(ui);
                if first {
                    // A returning user goes straight on.
                    let next = if self.settings.consented {
                        Page::Key
                    } else {
                        Page::Consent
                    };
                    self.show(ui, next);
                    self.say(Report::Ready);
                }
            }
            Event::Conversations(list) => {
                self.conversations = list;
                self.refresh_list();
                self.say(Report::Conversations(self.conversations.len()));
            }
            Event::Hits(hits) => {
                // A search that ran while the box was being cleared.
                if self.searching {
                    self.hits = Some(hits);
                    self.refresh_list();
                }
            }
            Event::Opened(conversation) => {
                if self.current_id().as_deref() != Some(conversation.id.as_str()) {
                    self.opened.clear();
                    self.drop_layouts();
                }
                // The error belonged to the turn before this one (a failed
                // turn sends its conversation first).
                self.failure = None;
                self.redo = Redo::No;
                self.pending_user = None;
                self.live = Live::default();
                self.chat.set_title(&title_of(&conversation));
                self.chat.set_status(&short_model(&conversation.model));
                self.current = Some(conversation);
                self.rows_dirty = true;
            }
            Event::Cleared => {
                self.drop_layouts();
                self.redo = Redo::No;
                self.current = None;
                self.pending_user = None;
                self.live = Live::default();
                self.failure = None;
                self.opened.clear();
                self.chat.set_title("New chat");
                self.chat.set_banner("");
                self.rows_dirty = true;
                self.scroll_to_end();
            }
            Event::Busy(busy) => {
                self.set_busy(busy);
                if !busy {
                    self.turn_started = None;
                }
            }
            Event::Text(text) => {
                self.note_first_chunk();
                self.live.text.push_str(&text);
                self.rows_dirty = true;
            }
            Event::Reasoning(text) => {
                self.note_first_chunk();
                self.live.reasoning.push_str(&text);
                self.rows_dirty = true;
            }
            Event::Retrying {
                attempt,
                seconds,
                why,
            } => {
                self.chat.set_banner(&format!(
                    "Retrying in {seconds} s (attempt {attempt}): {why}"
                ));
            }
            Event::Compacted(turns) => {
                self.chat.set_banner(&format!(
                    "The oldest {turns} turns were summarised to make room."
                ));
            }
            Event::Done(stats) => {
                let usage = stats.usage.unwrap_or_default();
                let first = stats
                    .first_chunk_ms
                    .map_or(String::new(), |ms| format!("first text after {ms} ms, "));
                self.chat.set_banner(&format!(
                    "{first}{} ms in all, {} tokens in, {} out{}",
                    stats.total_ms,
                    usage.prompt,
                    usage.completion,
                    if stats.truncated {
                        " (cut at the size limit)"
                    } else {
                        ""
                    }
                ));
                self.say(Report::Done {
                    prompt: usage.prompt,
                    completion: usage.completion,
                    ms: stats.total_ms,
                });
            }
            Event::Failed(failure) => {
                // A turn that failed before it began has no Busy(false) of its own.
                self.set_busy(false);
                self.turn_started = None;
                self.redo = Redo::No;
                // The message never reached the conversation: give it back.
                if let Some(text) = self.pending_user.take() {
                    if self.chat.draft_is_empty() {
                        self.chat.set_draft(&text);
                    }
                }
                self.live = Live::default();
                self.chat.set_banner(&failure.message);
                if failure.status != "stopped" {
                    self.failure = Some(failure.message.clone());
                }
                self.rows_dirty = true;
                self.say(Report::Failed(failure.status.clone()));
                if failure.key_needed {
                    self.have_key = false;
                    self.key.say(&failure.message);
                    self.show(ui, Page::Key);
                }
            }
            Event::Titled { id, title } => {
                if self.current_id().as_deref() == Some(id.as_str()) {
                    self.chat.set_title(&title);
                }
            }
            Event::Exported { .. } => {}
            Event::Notice(line) => self.chat.set_banner(&line),
        }
    }

    fn note_first_chunk(&mut self) {
        if !self.first_chunk {
            self.first_chunk = true;
            if let Some(started) = self.turn_started {
                self.say(Report::FirstChunk(started.elapsed().as_millis() as u64));
            }
        }
    }
}

fn short_model(id: &str) -> String {
    PRESETS.iter().find(|m| m.id == id).map_or_else(
        || id.rsplit('/').next().unwrap_or(id).to_string(),
        |m| m.name.to_string(),
    )
}

/// The conversation's title, or the first words of its first message.
fn title_of(conversation: &Conversation) -> String {
    if let Some(title) = &conversation.title {
        return title.clone();
    }
    conversation
        .items
        .iter()
        .find_map(|item| match item {
            lazyagent_core::store::Item::User { text } => {
                Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            _ => None,
        })
        .map_or_else(
            || "New chat".to_string(),
            |text| text.chars().take(48).collect(),
        )
}

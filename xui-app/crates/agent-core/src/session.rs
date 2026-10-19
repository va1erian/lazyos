//! One conversation being talked to: the user's message goes in, the stored
//! events and the streamed answer come out (docs/ai-agent-plan.md §4).
//! P1 has no tools; the agent loop with its budgets is P5 and builds on
//! [`Session::respond`].

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::compact;
use crate::message::ToolSpec;
use crate::provider::{ChatError, ChatRequest, Completion, Params, Provider, Sink};
use crate::store::{Conversation, Event, Store};

/// Why a turn did not finish.
#[derive(Debug)]
pub enum TurnError {
    /// The provider failed; the user's message is stored and can be retried.
    Chat(ChatError),
    /// The conversation file could not be written.
    Store(io::Error),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnError::Chat(e) => e.fmt(f),
            TurnError::Store(e) => write!(f, "cannot save the conversation: {e}"),
        }
    }
}

impl std::error::Error for TurnError {}

impl From<ChatError> for TurnError {
    fn from(e: ChatError) -> TurnError {
        TurnError::Chat(e)
    }
}

impl From<io::Error> for TurnError {
    fn from(e: io::Error) -> TurnError {
        TurnError::Store(e)
    }
}

pub struct Session<'a> {
    pub conversation: Conversation,
    store: &'a Store,
    provider: &'a dyn Provider,
    system_prompt: String,
    pub params: Params,
    pub tools: Vec<ToolSpec>,
}

impl<'a> Session<'a> {
    pub fn new(
        conversation: Conversation,
        store: &'a Store,
        provider: &'a dyn Provider,
        system_prompt: impl Into<String>,
    ) -> Session<'a> {
        Session {
            conversation,
            store,
            provider,
            system_prompt: system_prompt.into(),
            params: Params::default(),
            tools: Vec::new(),
        }
    }

    /// Writes `event` to the file, then applies it. A failed write leaves the
    /// conversation as it was.
    pub fn record(&mut self, event: Event) -> io::Result<()> {
        self.store.append(&self.conversation.id, &event)?;
        self.conversation.apply(event);
        Ok(())
    }

    /// Adds the user's message and answers it.
    pub fn send(
        &mut self,
        text: &str,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, TurnError> {
        self.record(Event::User {
            text: text.to_string(),
        })?;
        self.respond(stop, sink)
    }

    /// Answers the conversation as it stands: compacts first when the
    /// prompt would not fit, streams the reply and stores it. An answer that
    /// was cut short is stored too, marked interrupted, before the error is
    /// returned.
    pub fn respond(
        &mut self,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, TurnError> {
        if compact::needed(&self.conversation, &self.system_prompt) {
            let event = compact::summarize(self.provider, &self.conversation, stop)?;
            if let Event::Compact { covered_turns, .. } = &event {
                sink.compacted(*covered_turns);
            }
            self.record(event)?;
        }
        let request = ChatRequest {
            model: self.conversation.model.clone(),
            messages: self.conversation.messages(&self.system_prompt),
            tools: self.tools.clone(),
            params: self.params.clone(),
        };
        match self.provider.stream(&request, stop, sink) {
            Ok(done) => {
                self.record_answer(&done, false)?;
                Ok(done)
            }
            Err(error) => {
                if let Some(partial) = error.partial() {
                    self.record_answer(&partial.clone(), true)?;
                }
                Err(error.into())
            }
        }
    }

    /// Redoes the last answer: the turn is dropped and asked again.
    pub fn regenerate(
        &mut self,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, TurnError> {
        let Some(text) = self.last_user_text() else {
            return Err(ChatError::Refused("there is nothing to redo".into()).into());
        };
        self.edit_last(&text, stop, sink)
    }

    /// Rewrites the last user message and replays the turn; earlier turns
    /// stay (there is no branching in v1).
    pub fn edit_last(
        &mut self,
        text: &str,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, TurnError> {
        let turns = self.conversation.turns();
        if turns == 0 {
            return Err(ChatError::Refused("there is nothing to edit".into()).into());
        }
        self.record(Event::Rewind {
            keep_turns: turns - 1,
        })?;
        self.send(text, stop, sink)
    }

    /// Has the model name the conversation, once, after its first exchange.
    /// A failure is not worth the user's attention: the title stays the first
    /// words.
    pub fn name(&mut self, stop: &Arc<AtomicBool>) -> Option<String> {
        if self.conversation.title.is_some() {
            return None;
        }
        let title = compact::suggest_title(self.provider, &self.conversation, stop).ok()?;
        self.record(Event::Title {
            title: title.clone(),
        })
        .ok()?;
        Some(title)
    }

    fn last_user_text(&self) -> Option<String> {
        self.conversation
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                crate::store::Item::User { text } => Some(text.clone()),
                _ => None,
            })
    }

    fn record_answer(&mut self, done: &Completion, interrupted: bool) -> io::Result<()> {
        if !done.reasoning.is_empty() {
            self.record(Event::Reasoning {
                text: done.reasoning.clone(),
            })?;
        }
        if !done.content.is_empty() || !done.tool_calls.is_empty() || interrupted {
            if done.content.is_empty() && done.tool_calls.is_empty() && done.reasoning.is_empty() {
                // Nothing arrived before the break: no empty answer is stored.
                return Ok(());
            }
            self.record(Event::Assistant {
                text: done.content.clone(),
                model: self.conversation.model.clone(),
                interrupted,
            })?;
        }
        for call in &done.tool_calls {
            self.record(Event::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })?;
        }
        if let Some(usage) = done.usage {
            self.record(Event::Usage {
                prompt: usage.prompt,
                completion: usage.completion,
                reasoning: usage.reasoning,
            })?;
        }
        Ok(())
    }
}

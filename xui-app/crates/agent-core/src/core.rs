//! The core a front end drives (docs/ai-agent-plan.md §2): a worker thread
//! owns the HTTPS client, the key (in memory only, §5.3), the conversation
//! store and the current conversation; the window sends [`Request`]s and
//! drains [`Event`]s. Neither [`Core::send`] nor [`Events::try_next`] ever
//! blocks, and [`Core::stop`] works while a reply is streaming (it sets the
//! flag the stream looks at before every read).
//!
//! A streamed reply arrives as [`Event::Text`] and [`Event::Reasoning`]
//! deltas; when the turn ends the whole conversation follows as
//! [`Event::Opened`], so the window never has to merge by hand.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::provider::{ChatError, Completion, Fireworks, Params, Sink};
use crate::session::{Session, TurnError};
use crate::settings::Settings;
use crate::store::{Conversation, Event as StoredEvent, Hit, Persona, Store, Summary};
use crate::{Platform, Secret, Usage};

/// The system prompt of a plain chat.
pub const CHAT_PROMPT: &str = "You are LazyAgent, an assistant inside the LazyOS desktop. \
Answer clearly and keep it short unless asked for more. Write Markdown: fenced code blocks for \
code, lists for steps. Say so when you are not sure.";

#[derive(Debug, Clone)]
pub enum Request {
    /// The API key, for this run only.
    SetKey(Secret),
    ForgetKey,
    List,
    Search(String),
    /// Starts an empty conversation (nothing is stored until a message is sent).
    New,
    Open(String),
    Delete(String),
    Rename {
        id: String,
        title: String,
    },
    Send(String),
    Regenerate,
    /// Rewrites the last user message and asks again.
    EditLast(String),
    SaveSettings(Settings),
    Export(String),
    /// Writes the conversation as Markdown to `path` (the window picked it);
    /// the answer is a [`Event::Notice`].
    ExportTo {
        id: String,
        path: std::path::PathBuf,
    },
}

#[derive(Debug, Clone)]
pub enum Event {
    Settings(Settings),
    Conversations(Vec<Summary>),
    Hits(Vec<Hit>),
    /// The current conversation, whole (after opening one or after a turn).
    Opened(Box<Conversation>),
    /// No conversation is current (a new chat, or the open one was deleted).
    Cleared,
    /// A turn started or ended.
    Busy(bool),
    Text(String),
    Reasoning(String),
    Retrying {
        attempt: u32,
        seconds: u32,
        why: String,
    },
    Compacted(usize),
    Done(Stats),
    Failed(Failure),
    Titled {
        id: String,
        title: String,
    },
    Exported {
        id: String,
        markdown: String,
    },
    /// A line for the status bar.
    Notice(String),
}

/// What one answer cost and how fast it came (the P0 numbers).
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub first_chunk_ms: Option<u64>,
    pub total_ms: u64,
    pub usage: Option<Usage>,
    pub finish: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// `401`, `429`, `interrupted`, `stopped`, `transport`, ...
    pub status: String,
    pub message: String,
    /// The key is missing or was refused: ask for it again.
    pub key_needed: bool,
    /// Some of the answer arrived and is kept (marked interrupted).
    pub kept_partial: bool,
}

pub struct Core {
    requests: Sender<Option<Request>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

pub struct Events {
    rx: Receiver<Event>,
}

impl Events {
    pub fn try_next(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Waits up to `timeout` for the next event (tests, the host CLI).
    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Core {
    /// Starts the worker. The settings are read from the platform's app
    /// folder and announced as the first event.
    pub fn start(platform: Arc<dyn Platform>) -> (Core, Events) {
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("agent-llm".into())
            .spawn(move || Worker::new(platform, event_tx, flag).run(request_rx))
            .expect("the worker thread starts");
        (
            Core {
                requests,
                stop,
                worker: Some(worker),
            },
            Events { rx },
        )
    }

    /// Queues `request`; never blocks.
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(Some(request));
    }

    /// Ends a streaming reply within a poll. Never blocks.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.requests.send(None);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Worker {
    platform: Arc<dyn Platform>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
    store: Store,
    settings: Settings,
    client: Option<Fireworks>,
    current: Option<Conversation>,
}

/// Forwards a stream to the window.
struct Forward<'a>(&'a Sender<Event>);

impl Sink for Forward<'_> {
    fn text(&mut self, text: &str) {
        let _ = self.0.send(Event::Text(text.to_string()));
    }

    fn reasoning(&mut self, text: &str) {
        let _ = self.0.send(Event::Reasoning(text.to_string()));
    }

    fn retrying(&mut self, attempt: u32, delay: Duration, why: &str) {
        let _ = self.0.send(Event::Retrying {
            attempt,
            seconds: delay.as_secs().max(1) as u32,
            why: why.to_string(),
        });
    }

    fn compacted(&mut self, turns: usize) {
        let _ = self.0.send(Event::Compacted(turns));
    }
}

enum Turn {
    Send(String),
    Regenerate,
    Edit(String),
}

impl Worker {
    fn new(platform: Arc<dyn Platform>, events: Sender<Event>, stop: Arc<AtomicBool>) -> Worker {
        let dir = platform.app_dir();
        let worker = Worker {
            store: Store::new(&dir),
            settings: Settings::load(&dir),
            platform,
            events,
            stop,
            client: None,
            current: None,
        };
        worker.emit(Event::Settings(worker.settings.clone()));
        worker.emit(Event::Conversations(worker.store.list()));
        worker
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn run(mut self, requests: Receiver<Option<Request>>) {
        while let Ok(Some(request)) = requests.recv() {
            self.handle(request);
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::SetKey(key) => {
                if key.is_empty() {
                    self.client = None;
                } else {
                    self.client = Some(Fireworks::new(
                        self.platform.ca_roots(),
                        key,
                        self.platform.endpoint(),
                    ));
                }
            }
            Request::ForgetKey => self.client = None,
            Request::List => self.emit(Event::Conversations(self.store.list())),
            Request::Search(query) => self.emit(Event::Hits(self.store.search(&query))),
            Request::New => {
                self.current = None;
                self.emit(Event::Cleared);
            }
            Request::Open(id) => match self.store.load(&id) {
                Ok(conversation) => {
                    self.emit(Event::Opened(Box::new(conversation.clone())));
                    self.current = Some(conversation);
                }
                Err(e) => self.emit(Event::Notice(format!("cannot open the conversation: {e}"))),
            },
            Request::Delete(id) => {
                if let Err(e) = self.store.delete(&id) {
                    self.emit(Event::Notice(format!(
                        "cannot delete the conversation: {e}"
                    )));
                } else if self.current.as_ref().is_some_and(|c| c.id == id) {
                    self.current = None;
                    self.emit(Event::Cleared);
                }
                self.emit(Event::Conversations(self.store.list()));
            }
            Request::Rename { id, title } => self.rename(&id, &title),
            Request::Send(text) => self.turn(Turn::Send(text)),
            Request::Regenerate => self.turn(Turn::Regenerate),
            Request::EditLast(text) => self.turn(Turn::Edit(text)),
            Request::SaveSettings(mut settings) => {
                settings.normalise();
                if let Err(e) = settings.save(&self.platform.app_dir()) {
                    self.emit(Event::Notice(format!("cannot save the settings: {e}")));
                }
                self.settings = settings.clone();
                self.emit(Event::Settings(settings));
            }
            Request::ExportTo { id, path } => {
                let result = self
                    .store
                    .load(&id)
                    .and_then(|c| std::fs::write(&path, c.to_markdown()));
                self.emit(Event::Notice(match result {
                    Ok(()) => format!("Saved {}", path.display()),
                    Err(e) => format!("cannot export: {e}"),
                }));
            }
            Request::Export(id) => match self.store.load(&id) {
                Ok(conversation) => self.emit(Event::Exported {
                    id,
                    markdown: conversation.to_markdown(),
                }),
                Err(e) => self.emit(Event::Notice(format!("cannot export: {e}"))),
            },
        }
    }

    fn rename(&mut self, id: &str, title: &str) {
        let title: String = title
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        if title.is_empty() {
            return;
        }
        let event = StoredEvent::Title {
            title: title.clone(),
        };
        if self.store.append(id, &event).is_ok() {
            if let Some(current) = self.current.as_mut().filter(|c| c.id == id) {
                current.apply(event);
            }
            self.emit(Event::Titled {
                id: id.to_string(),
                title,
            });
            self.emit(Event::Conversations(self.store.list()));
        }
    }

    fn params(&self) -> Params {
        Params {
            temperature: self.settings.temperature,
            max_tokens: self.settings.max_tokens,
            reasoning_effort: self.settings.reasoning_effort(),
        }
    }

    fn system_prompt(&self) -> String {
        let mut prompt = CHAT_PROMPT.to_string();
        if !self.settings.system.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(self.settings.system.trim());
        }
        prompt
    }

    /// Runs one turn on the current conversation (making one when needed).
    fn turn(&mut self, turn: Turn) {
        if self.client.is_none() {
            self.emit(Event::Failed(Failure {
                status: "key".into(),
                message: "Enter your Fireworks API key to chat.".into(),
                key_needed: true,
                kept_partial: false,
            }));
            return;
        }
        let conversation = match self.current.take() {
            Some(conversation) => conversation,
            None => match self.store.create(
                self.platform.now(),
                &self.settings.model,
                Persona::Chat,
                false,
                None,
            ) {
                Ok(conversation) => conversation,
                Err(e) => {
                    self.emit(Event::Failed(Failure {
                        status: "store".into(),
                        message: format!("cannot start a conversation: {e}"),
                        key_needed: false,
                        kept_partial: false,
                    }));
                    return;
                }
            },
        };
        self.stop.store(false, Ordering::SeqCst);
        self.emit(Event::Busy(true));
        let prompt = self.system_prompt();
        let params = self.params();
        let model = self.settings.model.clone();
        let client = self.client.as_ref().expect("checked above");
        let mut session = Session::new(conversation, &self.store, client, prompt);
        session.params = params;
        if session.conversation.model != model {
            let _ = session.record(StoredEvent::Model { model });
        }
        let mut sink = Forward(&self.events);
        let result = match turn {
            Turn::Send(text) => session.send(&text, &self.stop, &mut sink),
            Turn::Regenerate => session.regenerate(&self.stop, &mut sink),
            Turn::Edit(text) => session.edit_last(&text, &self.stop, &mut sink),
        };
        let title = match &result {
            Ok(_) => session.name(&self.stop),
            Err(_) => None,
        };
        let conversation = session.conversation;
        self.finish(result, conversation, title);
    }

    fn finish(
        &mut self,
        result: Result<Completion, TurnError>,
        conversation: Conversation,
        title: Option<String>,
    ) {
        self.emit(Event::Opened(Box::new(conversation.clone())));
        if let Some(title) = title {
            self.emit(Event::Titled {
                id: conversation.id.clone(),
                title,
            });
        }
        match result {
            Ok(done) => self.emit(Event::Done(Stats {
                first_chunk_ms: done.first_chunk_ms,
                total_ms: done.total_ms,
                usage: done.usage,
                finish: done.finish_reason,
                truncated: done.truncated,
            })),
            Err(TurnError::Chat(error)) => {
                let key_needed = matches!(error, ChatError::Unauthorized(_));
                if key_needed {
                    // A refused key is not kept: it is asked for again.
                    self.client = None;
                }
                self.emit(Event::Failed(Failure {
                    status: error.status(),
                    message: error.to_string(),
                    key_needed,
                    kept_partial: error.partial().is_some(),
                }));
            }
            Err(error @ TurnError::Store(_)) => self.emit(Event::Failed(Failure {
                status: "store".into(),
                message: error.to_string(),
                key_needed: false,
                kept_partial: false,
            })),
        }
        self.current = Some(conversation);
        self.emit(Event::Conversations(self.store.list()));
        self.emit(Event::Busy(false));
    }
}

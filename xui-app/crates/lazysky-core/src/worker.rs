//! The `api` thread: requests in order, one HTTP client, one session.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::net::api::{Api, ApiError, Notes};
use crate::net::{Http, HttpOptions};
use crate::proto::Page;
use crate::{Account, Event, FeedSource, Platform, Request, Settings};

/// A page is this many posts.
const PAGE_SIZE: &str = "30";

pub(crate) enum Message {
    Request(Request),
    Shutdown,
}

struct EventNotes<'a>(&'a Sender<Event>);

impl Notes for EventNotes<'_> {
    fn status(&self, line: &str) {
        let _ = self.0.send(Event::Status {
            line: line.to_string(),
        });
    }
}

pub(crate) fn run(
    platform: Arc<dyn Platform>,
    settings: Settings,
    requests: Receiver<Message>,
    events: Sender<Event>,
    closing: Arc<AtomicBool>,
) {
    // Made on this thread: its sockets are this thread's own.
    let http = Http::new(platform.ca_roots(), HttpOptions::default());
    let mut api = Api::new(http, &settings.service, closing);
    while let Ok(Message::Request(request)) = requests.recv() {
        let event = handle(&mut api, platform.as_ref(), &events, request);
        if events.send(event).is_err() {
            break;
        }
    }
}

fn handle(
    api: &mut Api,
    platform: &dyn Platform,
    events: &Sender<Event>,
    request: Request,
) -> Event {
    let notes = EventNotes(events);
    match request {
        Request::Login {
            seq,
            handle,
            password,
        } => match api.sign_in(&handle, password.expose()) {
            Ok(session) => {
                let account = Account {
                    did: session.did.clone(),
                    handle: session.handle.clone(),
                };
                platform.log(&format!("SKY:LOGIN:PASS:{}", account.handle));
                Event::LoggedIn { seq, account }
            }
            Err(error) => {
                let reason = error.describe();
                platform.log(&format!("SKY:LOGIN:FAIL:{reason}"));
                Event::LoginFailed { seq, reason }
            }
        },
        Request::Logout { seq } => {
            api.sign_out();
            Event::LoggedOut { seq }
        }
        Request::Feed {
            seq,
            source,
            cursor,
        } => {
            let result = fetch_feed(api, &source, cursor.as_deref(), &notes);
            match result {
                Ok(page) => {
                    platform.log(&format!(
                        "SKY:FEED:PAGE:{}:{}",
                        page.items.len(),
                        page.skipped
                    ));
                    Event::Feed {
                        seq,
                        source,
                        cursor,
                        page,
                    }
                }
                Err(error) => failed(seq, &error),
            }
        }
    }
}

fn fetch_feed(
    api: &mut Api,
    source: &FeedSource,
    cursor: Option<&str>,
    notes: &dyn Notes,
) -> Result<Page, ApiError> {
    let mut query = vec![("limit", PAGE_SIZE)];
    if let Some(cursor) = cursor {
        query.push(("cursor", cursor));
    }
    match source {
        FeedSource::Timeline => {
            let value = api.get("app.bsky.feed.getTimeline", &query, notes)?;
            Ok(Page::from_feed(&value))
        }
    }
}

fn failed(seq: u64, error: &ApiError) -> Event {
    Event::Failed {
        seq,
        reason: error.describe(),
        signed_out: matches!(error, ApiError::SessionExpired | ApiError::NotSignedIn),
    }
}

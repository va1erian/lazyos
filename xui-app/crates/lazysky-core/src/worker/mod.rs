//! The `api` thread: requests in order, one HTTP client, one session.

mod feeds;
mod writes;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::net::api::{Api, ApiError, Notes};
use crate::net::{Http, HttpOptions};
use crate::{Account, Event, Platform, Request, Settings};

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
        } => match feeds::feed(api, &source, cursor.as_deref(), &notes) {
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
        },
        Request::Thread { seq, uri } => match feeds::thread(api, &uri, &notes) {
            Ok(thread) => Event::Thread { seq, thread },
            Err(error) => failed(seq, &error),
        },
        Request::Actors { seq, list, cursor } => {
            match feeds::actors(api, &list, cursor.as_deref(), &notes) {
                Ok((actors, cursor)) => Event::Actors {
                    seq,
                    actors,
                    cursor,
                },
                Err(error) => failed(seq, &error),
            }
        }
        Request::Profile { seq, actor } => match feeds::profile(api, &actor, &notes) {
            Ok(profile) => Event::Profile { seq, profile },
            Err(error) => failed(seq, &error),
        },
        Request::Notifications { seq, cursor } => {
            match feeds::notifications(api, cursor.as_deref(), &notes) {
                Ok((items, cursor)) => Event::Notifications { seq, items, cursor },
                Err(error) => failed(seq, &error),
            }
        }
        Request::UnreadCount { seq } => match feeds::unread(api, &notes) {
            Ok(count) => Event::Unread { seq, count },
            Err(error) => failed(seq, &error),
        },
        Request::MarkSeen { seq } => match writes::mark_seen(api, &notes) {
            Ok(()) => Event::Done { seq },
            Err(error) => failed(seq, &error),
        },
        Request::Configure { seq, service } => {
            api.set_service(&service);
            Event::Done { seq }
        }
        Request::SaveSettings { seq, settings } => match settings.save(&platform.app_dir()) {
            Ok(path) => {
                platform.log(&format!("SKY:SETTINGS:SAVED:{}", path.display()));
                Event::Done { seq }
            }
            Err(reason) => Event::Failed {
                seq,
                reason: format!("settings not saved: {reason}"),
                signed_out: false,
            },
        },
        Request::Raw { seq, json } => Event::Raw {
            seq,
            text: serde_json::to_string_pretty(&*json).unwrap_or_default(),
        },
        Request::Like { seq, subject } => writes::react(
            api,
            platform,
            &notes,
            seq,
            crate::proto::record::LIKE,
            &subject,
        ),
        Request::Repost { seq, subject } => writes::react(
            api,
            platform,
            &notes,
            seq,
            crate::proto::record::REPOST,
            &subject,
        ),
        Request::DeleteRecord { seq, uri } => writes::delete(api, platform, &notes, seq, &uri),
        Request::CreatePost {
            seq,
            text,
            reply,
            pictures,
        } => writes::post(api, platform, &notes, seq, &text, reply, &pictures),
    }
}

pub(crate) fn failed(seq: u64, error: &ApiError) -> Event {
    Event::Failed {
        seq,
        reason: error.describe(),
        signed_out: matches!(error, ApiError::SessionExpired | ApiError::NotSignedIn),
    }
}

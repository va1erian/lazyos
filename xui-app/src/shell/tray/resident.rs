//! Default items (docs/tray-plan.md section 6.2): every running resident app
//! of this session has an item, whether or not it set one. `init` publishes
//! the session's resident apps on the retained `session/<s>/apps/resident`
//! topic; the tray follows it, so a resident app with no tray code at all is
//! still visible, reopenable and quittable, and a restarted shell has its
//! default items back at once.
//!
//! Serial: `SHELL:TRAY:RESIDENT apps=<n>`, `SHELL:TRAY:DEFAULT app=<id>` when
//! an app gets its entry from the topic, `SHELL:TRAY:CLEAR app=<id>
//! why=stopped` when `init` reports it gone.

use lazyshell::tray::Change;
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::super::ctx::Ctx;
use crate::platform::topic_feed::TopicFeed;

/// Ticks between looks at the topic (a quarter second).
const LOOK_TICKS: u64 = 25;
/// Most resident apps taken from one value (the tray holds 64 items).
const MAX_APPS: usize = 64;

/// The session's resident-apps topic, followed.
pub struct ResidentFeed {
    feed: Option<TopicFeed>,
}

impl ResidentFeed {
    pub fn new(session: Option<u64>) -> ResidentFeed {
        let feed = session
            .and_then(|session| init_wire::name_session_apps_resident(&session.to_string()).ok())
            .map(|topic| TopicFeed::new(topic, topics::QOS_LATEST, 1, LOOK_TICKS));
        ResidentFeed { feed }
    }
}

/// Apply the newest list; `true` when the tray changed.
pub fn pump(ctx: &Ctx) -> bool {
    let events = match ctx.tray.resident.borrow_mut().feed.as_mut() {
        Some(feed) => feed.poll(),
        None => return false,
    };
    // Only the newest value matters: the topic is a state, not a log.
    let Some(value) = events
        .iter()
        .rev()
        .find_map(|event| init_wire::decode_session_apps_resident(&event.payload).ok())
    else {
        return false;
    };
    let running: Vec<(String, u64)> = value
        .apps
        .into_iter()
        .take(MAX_APPS)
        .map(|app| (app.app, app.pid))
        .collect();
    println!("SHELL:TRAY:RESIDENT apps={}", running.len());
    let changes = ctx.tray.model.borrow_mut().set_resident(&running);
    for (app, change) in &changes {
        match change {
            Change::Added => {
                if !ctx.tray.knows(app) {
                    ctx.tray.refresh_known(ctx);
                }
                println!("SHELL:TRAY:DEFAULT app={app}");
            }
            Change::Removed => {
                ctx.tray.drop_channel(app);
                println!("SHELL:TRAY:CLEAR app={app} why=stopped");
            }
            _ => {}
        }
    }
    !changes.is_empty()
}

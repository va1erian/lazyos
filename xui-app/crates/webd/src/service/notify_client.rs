//! A view made with `notify_in_client` (docs/webd-plan.md 3.5, open question 6, P4): the page's
//! notifications are shown by the app, not by webd, so a click on one brings the app's window
//! forward (the shell raises the app that posted the toast, and an app cannot raise itself).
//!
//! webd keeps the decisions per profile exactly as for its own toasts, and hands the app only what
//! the profile allowed (`NotifyShow`). The app answers each with `NotifyState` (shown, clicked,
//! closed); the page's own close and the end of its object come as `NotifyCancel` and `NotifyGone`.
//! webd remembers the ids it handed out, at most [`CLIENT_NOTIFICATIONS_PER_VIEW`] per view, so an
//! app can only speak about its own view's notifications and a page cannot grow the table.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::calls::{live, MALFORMED};
use super::{Note, Service};
use crate::config::{clip, Refusal};
use crate::engine::Engine;
use crate::fabric::{errno, Fabric};
use crate::ident::ClientKey;
use crate::limits::{CLIENT_NOTIFICATIONS_PER_VIEW, NOTIFY_TEXT_BYTES};

const NOT_GIVEN: Refusal = Refusal::new(errno::EACCES, "the view was not made with that");
const NO_SUCH: Refusal = Refusal::new(
    errno::ESRCH,
    "no such notification: over or not this view's",
);

impl<E: Engine, F: Fabric> Service<E, F> {
    /// Whether the app of `view` shows its page's notifications.
    pub(super) fn in_client(&mut self, view: u64) -> bool {
        self.full_of(view)
            .is_some_and(|full| full.rules.notify_in_client)
    }

    /// The page asked to show notification `id`; `texts` are its origin, page address, title, body
    /// and tag. Handed to the app when the view's profile allowed the origin and the view has room,
    /// else the page is told it closed at once.
    pub(super) fn forward_show(&mut self, view: u64, id: u64, texts: [String; 5]) {
        let [origin, page_url, title, body, tag] = texts;
        let profile = self.full_of(view).map(|full| full.rules.profile.clone());
        let allowed = profile.is_some_and(|p| self.decision(&p, &origin) == Some(true));
        let room = self
            .full_of(view)
            .is_some_and(|full| full.forwarded.len() < CLIENT_NOTIFICATIONS_PER_VIEW);
        if !(allowed && room) {
            self.engine.notify_closed(id);
            self.note(Note::Full {
                view,
                what: "notification refused",
            });
            return;
        }
        if let Some(full) = self.full_of(view) {
            full.forwarded.insert(id, true);
        }
        let cut = |text: &str| clip(text, NOTIFY_TEXT_BYTES).to_owned();
        let body = wire::encode_notify_show_args(&wire::NotifyShowArgs {
            view,
            notification: id,
            origin: cut(&origin),
            page_url: cut(&page_url),
            title: cut(&title),
            body: cut(&body),
            tag: cut(&tag),
        });
        // Not delivered (the app's inbox is full, the view is going): the page hears it closed, and
        // the id does not stay open for ever.
        if !self.event_to_live(view, wire::METHOD_NOTIFYSHOW, body) {
            if let Some(full) = self.full_of(view) {
                full.forwarded.remove(&id);
            }
            self.engine.notify_closed(id);
        }
    }

    /// The page closed notification `id` (`NotifyCancel`: it is over) or its object went away
    /// (`NotifyGone`: the app's toast stays and is still the app's to report). Only for an id the
    /// app was given.
    pub(super) fn forward_end(&mut self, view: u64, id: u64, method: u32) {
        let Some(full) = self.full_of(view) else {
            return;
        };
        let known = if method == wire::METHOD_NOTIFYCANCEL {
            full.forwarded.remove(&id).is_some()
        } else {
            full.forwarded
                .get_mut(&id)
                .map(|alive| *alive = false)
                .is_some()
        };
        if !known {
            return;
        }
        let body = if method == wire::METHOD_NOTIFYCANCEL {
            wire::encode_notify_cancel_args(&wire::NotifyCancelArgs {
                view,
                notification: id,
            })
        } else {
            wire::encode_notify_gone_args(&wire::NotifyGoneArgs {
                view,
                notification: id,
            })
        };
        self.event_to_live(view, method, body);
    }

    /// `NotifyState`: what became of the app's toast for notification `notification`.
    pub(super) fn notify_state_call(
        &mut self,
        client: ClientKey,
        body: &[u8],
    ) -> Result<(), Refusal> {
        let args = wire::decode_notify_state_args(body).map_err(|_| MALFORMED)?;
        if !matches!(
            args.state,
            wire::NOTIFY_STATE_KIND_SHOWN
                | wire::NOTIFY_STATE_KIND_CLICKED
                | wire::NOTIFY_STATE_KIND_CLOSED
        ) {
            return Err(MALFORMED);
        }
        let view = live(&mut self.views, client, true, args.view)?;
        let full = view.full.as_mut().ok_or(MALFORMED)?;
        if !full.rules.notify_in_client {
            return Err(NOT_GIVEN);
        }
        let id = args.notification;
        let alive = *full.forwarded.get(&id).ok_or(NO_SUCH)?;
        if args.state == wire::NOTIFY_STATE_KIND_CLOSED {
            full.forwarded.remove(&id);
        }
        // A page whose object is gone has no script to tell: the state only ends the entry.
        if alive {
            match args.state {
                wire::NOTIFY_STATE_KIND_SHOWN => self.engine.notify_shown(id),
                wire::NOTIFY_STATE_KIND_CLICKED => self.engine.notify_clicked(id),
                _ => self.engine.notify_closed(id),
            }
        }
        Ok(())
    }
}

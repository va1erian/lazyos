//! A full view is made: its config checked against the scope it came through (`fullconfig`), the
//! user's web process cap, then the table, the engine's rules and the engine.
//!
//! **Web processes.** The engine gives the views of one group and one profile one web process, and
//! the service gives each client one group, so the processes of a user's full views are the distinct
//! (client, profile) pairs among them. A view that would start a fifth is `EDQUOT`
//! ([`WEB_PROCESSES_PER_USER`]): about 107 MiB each, under one user memory quota. Light views are not
//! counted (one process per client, bounded by the view count).

use std::collections::BTreeSet;
use std::sync::Arc;

use lazydownloads::Saver;
use libmessenger::Object;
use messenger_generated::os_lazy_webd_view_v1 as view_wire;

use super::calls::{Reply, MALFORMED};
use super::{claim, Note, Service};
use crate::config::Refusal;
use crate::engine::Engine;
use crate::events::rules::ViewRules;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::{self, Profile, Scope};
use crate::ident::ClientKey;
use crate::limits::{VIEWS_PER_CLIENT, WEB_PROCESSES_PER_USER};
use crate::policy::Schemes;
use crate::view::{FullView, View};

impl<E: Engine, F: Fabric> Service<E, F> {
    /// The web processes `uid`'s full views hold: one per (client, profile, network lock): the engine
    /// groups a view with the network lock only with views that have it too.
    fn web_processes(&self, uid: u32) -> BTreeSet<(ClientKey, Profile, bool)> {
        self.views
            .iter()
            .filter(|(_, view)| view.owner.uid == uid && view.lost.is_none())
            .filter_map(|(_, view)| {
                let rules = &view.full.as_ref()?.rules;
                Some((
                    view.owner,
                    rules.profile.clone(),
                    rules.policy.schemes == Schemes::DataOnly,
                ))
            })
            .collect()
    }

    /// The browser profile's downloads, one saver for every view that may download.
    fn saver(&mut self, dir: &std::path::Path) -> Arc<Saver> {
        if let Some(saver) = &self.saver {
            return saver.clone();
        }
        let saver = Arc::new(Saver::new(dir.to_path_buf()));
        self.saver = Some(saver.clone());
        saver
    }

    pub(super) fn create_full_view(
        &mut self,
        client: ClientKey,
        scope: Scope,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        // A scope's `CreateView` has the full interface's shape (tools/webd/test_idl.py).
        let args = view_wire::decode_create_view_args(body, pending).map_err(|_| MALFORMED)?;
        if self.views.count_of(client) >= VIEWS_PER_CLIENT {
            return Err(Refusal::new(errno::ENOSPC, "too many views"));
        }
        // Checked before a group number is taken: a refused call from an unlabelled task must not
        // leave one behind.
        let mut setup = fullconfig::setup(
            &args.config,
            scope,
            args.width,
            args.height,
            0,
            &self.places,
        )?;
        let mut processes = self.web_processes(client.uid);
        let no_network = setup.rules.policy.schemes == Schemes::DataOnly;
        if processes.insert((client, setup.rules.profile.clone(), no_network))
            && processes.len() > WEB_PROCESSES_PER_USER
        {
            return Err(Refusal::new(errno::EDQUOT, "too many web processes"));
        }
        let group = self
            .groups
            .group_of(client)
            .ok_or(Refusal::new(errno::ENOSPC, "no web process group left"))?;
        setup.engine.group = group;
        let downloads = setup.rules.downloads.clone().map(|dir| self.saver(&dir));
        let engine_rules = ViewRules {
            policy: setup.rules.policy.clone(),
            profile: setup.rules.profile.clone(),
            blocking: setup.rules.blocking,
            downloads,
            popups: setup.rules.popups,
            owner: u64::try_from(group).unwrap_or(0),
        };
        let deltas = setup.rules.deltas;
        let mut view = View::new(client, args.events, args.width, args.height);
        view.full = Some(Box::new(FullView::new(setup.rules)));
        let id = self
            .views
            .insert(view)
            .ok_or(Refusal::new(errno::ENOSPC, "no view id left"))?;
        claim(pending, Object::Channel(args.events));
        // The rules are in place before the engine can ask about the view's first navigation.
        self.queue.rules().register(id, engine_rules);
        if deltas {
            self.queue.track_pictures(id);
        }
        self.engine.view_create(id, &setup.engine);
        self.note(Note::ViewCreated {
            view: id,
            client,
            width: args.width,
            height: args.height,
            group,
        });
        view_wire::encode_create_view_reply(&view_wire::CreateViewReply { view: id })
            .map_err(|_| MALFORMED)
    }
}

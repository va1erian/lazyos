//! A full view is made: its config checked against the scope it came through (`fullconfig`), the
//! user's web process cap, then the table, the engine's rules and the engine.
//!
//! **Web processes.** The engine gives the views of one group and one profile one web process, and
//! the service gives each client one group per process key (`ViewConfig.process_key`, `""` by
//! default), so the processes of a user's full views are the distinct (client, profile, key) triples
//! among them. A view that would start one past [`WEB_PROCESSES_PER_USER`] is `EDQUOT`, as is one that
//! would start a process while the user's memory charge cannot hold it: about 107 MiB each, under one
//! user memory quota. Light views are not counted (one process per client, bounded by the view count).

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
use crate::ident::GroupError;
use crate::limits::{
    VIEWS_PER_BROWSER_CLIENT, VIEWS_PER_CLIENT, WEB_PROCESSES_PER_USER, WEB_PROCESS_RESERVE_BYTES,
};
use crate::policy::Schemes;
use crate::view::{FullView, View};

fn group_refusal(why: GroupError) -> Refusal {
    match why {
        GroupError::TooManyKeys => Refusal::new(errno::EDQUOT, "too many process keys"),
        GroupError::NoNumber => Refusal::new(errno::ENOSPC, "no web process group left"),
    }
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// The web processes `uid`'s full views hold: one per (client, profile, network lock, process key):
    /// the engine groups a view with the network lock only with views that have it too, and the
    /// service gives each key a group of its own.
    fn web_processes(&self, uid: u32) -> BTreeSet<(ClientKey, Profile, bool, String)> {
        self.views
            .iter()
            .filter(|(_, view)| view.owner.uid == uid && view.lost.is_none())
            .filter_map(|(_, view)| {
                let rules = &view.full.as_ref()?.rules;
                Some((
                    view.owner,
                    rules.profile.clone(),
                    rules.policy.schemes == Schemes::DataOnly,
                    rules.process_key.clone(),
                ))
            })
            .collect()
    }

    /// Whether the user's memory charge is too low to start another web process (a margin of
    /// [`WEB_PROCESS_RESERVE_BYTES`] over the current charge would exceed the limit). `false` when the
    /// host cannot read the charge or reports no limit, so the fixed process cap is then the only gate.
    pub(super) fn memory_tight(&self) -> bool {
        // Pixel buffers kept for reuse (`events/spares.rs`) are not a reason to refuse a view.
        self.queue.drop_spares();
        match self.queue.memory_charge() {
            Some((used, limit)) if limit > 0 => {
                used.saturating_add(WEB_PROCESS_RESERVE_BYTES) > limit
            }
            _ => false,
        }
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
        // A browser holds a view per tab; the other scopes keep the small cap.
        let cap = if scope == Scope::Browser {
            VIEWS_PER_BROWSER_CLIENT
        } else {
            VIEWS_PER_CLIENT
        };
        if self.views.count_of(client) >= cap {
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
        let starts_process = processes.insert((
            client,
            setup.rules.profile.clone(),
            no_network,
            setup.rules.process_key.clone(),
        ));
        if starts_process && processes.len() > WEB_PROCESSES_PER_USER {
            return Err(Refusal::new(errno::EDQUOT, "too many web processes"));
        }
        // Memory pressure (docs/webd-plan.md 11 item 7): a view that would start a new web process is
        // refused when the user's memory charge cannot hold one more, rather than letting the child's
        // `exec` fail under the quota. Hidden background views already get no paints (set_visible); the
        // decision taken is to refuse the new view, not evict an old one (the client reconnects or
        // closes a view it no longer needs). When the host cannot read the charge the fixed cap above
        // is the only bound.
        if starts_process && self.memory_tight() {
            return Err(Refusal::new(
                errno::EDQUOT,
                "not enough memory for another web process",
            ));
        }
        // The keyed group first: it is the one that can be refused (too many keys) and nothing is made
        // before it. The shared saver limits downloads per client, so its owner is the client's default
        // group, one number per client whatever process keys its views have; it fails only when every
        // number is taken.
        let group = self
            .groups
            .group_for(client, &setup.rules.process_key)
            .map_err(group_refusal)?;
        let owner = self.groups.group_for(client, "").map_err(group_refusal)?;
        setup.engine.group = group;
        let downloads = setup.rules.downloads.clone().map(|dir| self.saver(&dir));
        let engine_rules = ViewRules {
            policy: setup.rules.policy.clone(),
            profile: setup.rules.profile.clone(),
            blocking: setup.rules.blocking,
            downloads,
            popups: setup.rules.popups,
            owner: u64::try_from(owner).unwrap_or(0),
        };
        let deltas = setup.rules.deltas;
        let mut view = View::new(client, args.events, args.width, args.height);
        let key = setup.rules.process_key.clone();
        view.full = Some(Box::new(FullView::new(setup.rules)));
        let Some(id) = self.views.insert(view) else {
            // Nothing holds the key's group now: it goes, as it would with the key's last view.
            self.release_key_if_unused(client, &key);
            return Err(Refusal::new(errno::ENOSPC, "no view id left"));
        };
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

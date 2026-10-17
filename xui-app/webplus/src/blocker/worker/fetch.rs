//! The worker's fetch: when a list is due, the `curl` child, and what its answer does.

use std::fs;
use std::sync::Arc;

use super::lists::ListSpec;
use super::{say, update, Cmd, ListState, Worker};
use crate::blocker::lists;

impl Worker {
    /// Fetches the list when it is due (or `force`), keeping a good copy on any failure. The
    /// download runs on a thread of its own once the worker is spawned, so the engine's answers
    /// (and a list that is already blocking) never wait behind a slow network.
    pub(super) fn fetch(&mut self, spec: &'static ListSpec, force: bool) {
        if !self.enabled(spec) || self.fetching.contains(spec.id) {
            return;
        }
        let now = (self.now)();
        let mut meta = self.read_meta(spec);
        if !force && !meta.due(now) {
            return;
        }
        let was_ready = self.state_of(spec) == Some(ListState::Ready);
        // A list that is compiling from the store keeps saying so: the fetch is not the news.
        if matches!(
            self.state_of(spec),
            Some(ListState::Waiting | ListState::Failed(_))
        ) {
            self.set_state(spec, ListState::Fetching);
        }
        meta.attempted = now;
        self.write_meta(spec, &meta);

        let download = self.path(spec, "dl");
        let args = update::curl_args(spec.url, &download.to_string_lossy(), &meta);
        let runner = Arc::clone(&self.runner);
        let work = move || -> Result<update::Reply, String> {
            let outcome = match runner.curl(&args) {
                Ok(exit) if exit.code == 0 => fs::read(&download)
                    .map_err(|e| format!("no answer: {e}"))
                    .and_then(|raw| {
                        update::parse_reply(&raw).ok_or_else(|| String::from("not an HTTP answer"))
                    }),
                Ok(exit) => Err(format!("curl exit {}: {}", exit.code, exit.note)),
                Err(e) => Err(format!("curl did not run: {e}")),
            };
            let _ = fs::remove_file(&download);
            outcome
        };
        if !self.background {
            let outcome = work();
            self.finish_fetch(spec, outcome, was_ready, now);
            return;
        }
        self.fetching.insert(spec.id);
        let blocker = self.blocker.clone();
        let id = spec.id.to_owned();
        let spawned = std::thread::Builder::new()
            .name(String::from("blocker-fetch"))
            .spawn(move || {
                let outcome = work();
                blocker.send(Cmd::Fetched {
                    id,
                    outcome,
                    was_ready,
                    now,
                });
            });
        if spawned.is_err() {
            self.fetching.remove(spec.id);
            self.fetch_failed(spec, "no thread to fetch with");
        }
    }

    /// What a fetch brought back, applied: the list kept, replaced or left alone.
    pub(super) fn finish_fetch(
        &mut self,
        spec: &'static ListSpec,
        outcome: Result<update::Reply, String>,
        was_ready: bool,
        now: u64,
    ) {
        let mut meta = self.read_meta(spec);
        match outcome {
            Ok(reply) if reply.status == 304 => {
                meta.fetched = now;
                self.write_meta(spec, &meta);
                self.blocker.update_list(spec.id, |l| l.fetched = Some(now));
                say(&format!("UPDATE:{}:NOTMODIFIED", spec.id));
                if !was_ready && self.state_of(spec) != Some(ListState::Ready) {
                    self.activate(spec);
                }
            }
            Ok(reply) if reply.status == 200 && update::looks_like_list(&reply.body) => {
                if !self.enabled(spec) {
                    // Switched off while the fetch ran: nothing is taken.
                    say(&format!("UPDATE:{}:DROPPED", spec.id));
                    return;
                }
                let text = String::from_utf8_lossy(&reply.body).into_owned();
                meta.fetched = now;
                meta.expires = update::expires_of(&text);
                meta.etag = reply.etag;
                meta.last_modified = reply.last_modified;
                say(&format!("UPDATE:{}:OK:bytes={}", spec.id, reply.body.len()));
                if lists::source_hash(&reply.body) == meta.hash && !meta.parts.is_empty() {
                    // The same list again: only the validators and the date change.
                    self.write_meta(spec, &meta);
                    self.blocker.update_list(spec.id, |l| l.fetched = Some(now));
                    if self.state_of(spec) != Some(ListState::Ready) {
                        self.activate(spec);
                    }
                } else {
                    // A new version is tried first: the list that works keeps blocking until
                    // the engine has taken every part of it.
                    self.compile(spec, &reply.body, Some(meta));
                }
            }
            Ok(reply) => {
                let why = if reply.status == 200 {
                    String::from("the answer is not a filter list")
                } else {
                    format!("HTTP {}", reply.status)
                };
                self.fetch_failed(spec, &why);
            }
            Err(why) => self.fetch_failed(spec, &why),
        }
    }

    /// A failed fetch changes nothing about a list that works or is being brought up from the
    /// store: only a list with nothing to use reports it.
    fn fetch_failed(&self, spec: &ListSpec, why: &str) {
        say(&format!(
            "UPDATE:{}:FAIL:{}",
            spec.id,
            why.replace(['\n', '\r'], " ")
        ));
        if matches!(
            self.state_of(spec),
            Some(ListState::Fetching | ListState::Waiting)
        ) {
            self.set_state(spec, ListState::Failed(why.to_owned()));
        }
    }
}

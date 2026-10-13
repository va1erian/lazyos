//! The blocker's worker: the one thread that touches the lists' files, runs the converter and
//! the fetch, and hands the result to the engine.
//!
//! Nothing here waits for a page: a list that is not ready blocks nothing and the browser works.
//! One list at a time moves through *fetch* (a `curl` child under its own label) and *activate*
//! (use what is on disk: ask the engine's store for the parts compiled last time, and only when
//! that fails convert and compile). The engine answers each part with
//! [`Cmd::ListResult`], which the window forwards. All steps are synchronous methods of
//! [`Worker`] so the host tests drive them with a fake runner and a recording engine.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use xui_webkit::abi::ListStatus;
use xui_webkit::engine::Engine;

use super::lists::{self, ListSpec, LISTS};
use super::update::{self, Meta};
use super::{Blocker, ListState};

/// What the worker is told.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// The window is up: use what is on disk, fetch what is due.
    Start,
    /// The user switched a list on or off.
    ListsChanged,
    /// Fetch now, whatever the schedule says ("Update now").
    UpdateNow,
    /// The engine's answer for one compiled part.
    ListResult {
        id: String,
        status: ListStatus,
    },
    /// A fetch that ran on a thread of its own ended: what it brought back, and the list's state
    /// when it began (a list that was working keeps working whatever the fetch says).
    Fetched {
        id: String,
        outcome: Result<update::Reply, String>,
        was_ready: bool,
        now: u64,
    },
    Stop,
}

/// How a fetch ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    /// What the program said on its error output, trimmed.
    pub note: String,
}

/// Runs `curl` (a child under the `listupdate` label on LazyOS).
pub trait Runner: Send + Sync {
    fn curl(&self, args: &[String]) -> io::Result<Exit>;
}

/// The worker's state. `now` is seconds since the epoch.
pub struct Worker {
    dir: PathBuf,
    engine: Arc<dyn Engine + Send + Sync>,
    runner: Arc<dyn Runner>,
    blocker: Blocker,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pending: HashMap<&'static str, Pending>,
    /// Fetches run on a thread of their own (the engine's answers must not wait behind a slow
    /// network): true once the worker is spawned; the tests run them in place.
    background: bool,
    /// Lists with a fetch under way.
    fetching: HashSet<&'static str>,
}

/// The parts of one list the engine has been asked for.
struct Pending {
    parts: Vec<String>,
    /// The parts the engine has answered Ready for.
    ready: HashSet<String>,
    /// Asked with `blocking_load` (the store may not have them) and not yet compiled from source.
    loading: bool,
    rules: usize,
    dropped: usize,
    /// A new version of the list that is only being tried: it replaces the one on disk, and the
    /// parts of the old one go, once every part of it is Ready. Until then the old list keeps
    /// blocking, and a version the engine refuses changes nothing.
    candidate: Option<Candidate>,
    /// The parts of the version in use, forgotten when the candidate is ready.
    superseded: Vec<String>,
}

/// A downloaded version of a list that has not been taken yet.
struct Candidate {
    source: Vec<u8>,
    /// What the record will say once it is taken: the validators of the download, the counts.
    meta: Meta,
}

fn say(line: &str) {
    crate::marker(&format!("BLOCK:{line}"));
}

impl Worker {
    pub fn new(
        dir: PathBuf,
        engine: Arc<dyn Engine + Send + Sync>,
        runner: Arc<dyn Runner>,
        blocker: Blocker,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Worker {
        Worker {
            dir,
            engine,
            runner,
            blocker,
            now,
            pending: HashMap::new(),
            background: false,
            fetching: HashSet::new(),
        }
    }

    /// Runs the worker on a thread of its own; the sender is the way to it (the blocker keeps it,
    /// so the window's events reach the worker through [`Blocker::send`]).
    pub fn spawn(mut self) -> Sender<Cmd> {
        self.background = true;
        let (commands, inbox) = channel();
        self.blocker.connect(commands.clone());
        let spawned = std::thread::Builder::new()
            .name(String::from("blocker"))
            .spawn(move || {
                let mut worker = self;
                for command in inbox {
                    if !worker.handle(command) {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            say("FAIL:no thread");
        }
        commands
    }

    /// Does what `command` asks. False when the worker should end.
    pub fn handle(&mut self, command: Cmd) -> bool {
        match command {
            Cmd::Start => {
                let _ = fs::create_dir_all(&self.dir);
                for spec in &LISTS {
                    self.activate(spec);
                    self.fetch(spec, false);
                }
            }
            Cmd::ListsChanged => {
                for spec in &LISTS {
                    self.activate(spec);
                    self.fetch(spec, false);
                }
            }
            Cmd::UpdateNow => {
                for spec in &LISTS {
                    if self.enabled(spec) {
                        self.fetch(spec, true);
                    }
                }
            }
            Cmd::ListResult { id, status } => self.result(&id, status),
            Cmd::Fetched {
                id,
                outcome,
                was_ready,
                now,
            } => {
                if let Some(spec) = lists::spec(&id) {
                    self.fetching.remove(spec.id);
                    self.finish_fetch(spec, outcome, was_ready, now);
                }
            }
            Cmd::Stop => return false,
        }
        true
    }

    /// Whether the user has `spec` switched on (its parts are forgotten when not).
    fn list_on(&self, spec: &ListSpec) -> bool {
        self.blocker
            .lists()
            .iter()
            .any(|l| l.id == spec.id && l.enabled)
    }

    /// Whether the blocker is on at all: when it is off nothing is fetched and nothing is loaded
    /// (the engine's lists stay as they are, and decide nothing).
    fn blocking_on(&self) -> bool {
        self.blocker.settings().enabled
    }

    fn enabled(&self, spec: &ListSpec) -> bool {
        self.blocking_on() && self.list_on(spec)
    }

    fn set_state(&self, spec: &ListSpec, state: ListState) {
        self.blocker.update_list(spec.id, |l| l.state = state);
    }

    fn path(&self, spec: &ListSpec, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", spec.id))
    }

    fn read_meta(&self, spec: &ListSpec) -> Meta {
        fs::read_to_string(self.path(spec, "meta"))
            .map(|text| Meta::parse(&text))
            .unwrap_or_default()
    }

    fn write_meta(&self, spec: &ListSpec, meta: &Meta) {
        if let Err(e) = fs::write(self.path(spec, "meta"), meta.render()) {
            say(&format!("FAIL:{}:meta:{e}", spec.id));
        }
    }

    /// Uses the list's file on disk: switches it off, loads what the engine's store holds, or
    /// converts and compiles.
    fn activate(&mut self, spec: &'static ListSpec) {
        let meta = self.read_meta(spec);
        if !self.list_on(spec) {
            self.pending.remove(spec.id);
            for part in &meta.parts {
                self.engine.blocking_forget(part);
            }
            self.set_state(spec, ListState::Waiting);
            return;
        }
        if !self.blocking_on() {
            return;
        }
        let too_large = fs::metadata(self.path(spec, "txt"))
            .is_ok_and(|m| m.len() > lists::MAX_SOURCE_BYTES as u64);
        if too_large {
            self.set_state(spec, ListState::Failed("the list file is too large".into()));
            return;
        }
        let Ok(source) = fs::read(self.path(spec, "txt")) else {
            self.set_state(spec, ListState::Waiting);
            return;
        };
        self.blocker.update_list(spec.id, |l| {
            l.fetched = (meta.fetched > 0).then_some(meta.fetched);
        });
        if meta.hash == lists::source_hash(&source) && !meta.parts.is_empty() {
            self.set_state(spec, ListState::Compiling);
            self.pending.insert(
                spec.id,
                Pending {
                    parts: meta.parts.clone(),
                    ready: HashSet::new(),
                    loading: true,
                    rules: meta.rules,
                    dropped: meta.dropped,
                    candidate: None,
                    superseded: Vec::new(),
                },
            );
            for part in &meta.parts {
                self.engine.blocking_load(part);
            }
        } else {
            self.compile(spec, &source, None);
        }
    }

    /// Converts `source` and gives the parts to the engine.
    ///
    /// `update` is a version just downloaded (its record, with the new validators): it is only
    /// tried, and replaces the list on disk and the parts in use when every part is Ready, so a
    /// version the engine refuses changes nothing. Without it `source` is what the disk already
    /// holds (the store lost its parts), and the record is written at once.
    fn compile(&mut self, spec: &'static ListSpec, source: &[u8], update: Option<Meta>) {
        let started = (self.now)();
        let prepared = lists::prepare(spec.id, source);
        let ids: Vec<String> = prepared.parts.iter().map(|(id, _)| id.clone()).collect();
        let on_disk = self.read_meta(spec);
        let mut meta = update.clone().unwrap_or_else(|| on_disk.clone());
        meta.hash = prepared.hash.clone();
        meta.parts = ids.clone();
        meta.rules = prepared.rules();
        meta.dropped = prepared.dropped();
        say(&format!(
            "CONVERT:{}:rules={}:dropped={}:parts={}:t={}",
            spec.id,
            meta.rules,
            meta.dropped,
            ids.len(),
            (self.now)().saturating_sub(started)
        ));
        let stale: Vec<String> = on_disk
            .parts
            .iter()
            .filter(|old| !ids.contains(old))
            .cloned()
            .collect();
        if ids.is_empty() {
            let why = prepared
                .refused
                .unwrap_or("the list has no rule the engine can use");
            self.refuse(spec, &[], why, update.is_some());
            return;
        }
        let candidate = update.is_some();
        if candidate {
            // The list that works keeps working until the new one is ready.
            if self.state_of(spec) != Some(ListState::Ready) {
                self.set_state(spec, ListState::Compiling);
            }
        } else {
            for old in &stale {
                self.engine.blocking_forget(old);
            }
            self.write_meta(spec, &meta);
            self.set_state(spec, ListState::Compiling);
        }
        self.pending.insert(
            spec.id,
            Pending {
                parts: ids,
                ready: HashSet::new(),
                loading: false,
                rules: meta.rules,
                dropped: meta.dropped,
                candidate: candidate.then(|| Candidate {
                    source: source.to_vec(),
                    meta,
                }),
                superseded: if candidate { stale } else { Vec::new() },
            },
        );
        for (id, json) in &prepared.parts {
            self.engine.blocking_compile(id, json);
        }
    }

    fn state_of(&self, spec: &ListSpec) -> Option<ListState> {
        self.blocker
            .lists()
            .iter()
            .find(|l| l.id == spec.id)
            .map(|l| l.state.clone())
    }

    /// The engine took every part of a version that was only being tried: it becomes the list on
    /// disk, and the parts it replaces go.
    fn promote(&mut self, spec: &'static ListSpec) {
        let Some(p) = self.pending.get_mut(spec.id) else {
            return;
        };
        let (Some(candidate), superseded) = (p.candidate.take(), std::mem::take(&mut p.superseded))
        else {
            return;
        };
        let staging = self.path(spec, "new");
        let stored = fs::write(&staging, &candidate.source)
            .and_then(|()| fs::rename(&staging, self.path(spec, "txt")));
        match stored {
            Ok(()) => self.write_meta(spec, &candidate.meta),
            Err(e) => say(&format!("FAIL:{}:keep:{e}", spec.id)),
        }
        let fetched = (candidate.meta.fetched > 0).then_some(candidate.meta.fetched);
        self.blocker.update_list(spec.id, |l| l.fetched = fetched);
        for old in &superseded {
            self.engine.blocking_forget(old);
        }
    }

    /// A version the engine (or the converter) refused. A version that was only being tried
    /// leaves the list that works in place; otherwise the list fails.
    fn refuse(&mut self, spec: &'static ListSpec, parts: &[String], why: &str, was_trial: bool) {
        self.pending.remove(spec.id);
        if was_trial && self.state_of(spec) == Some(ListState::Ready) {
            // The parts that did compile are no use to anyone.
            for part in parts {
                self.engine.blocking_forget(part);
            }
            say(&format!("REFUSED:{}:{why}", spec.id));
            return;
        }
        self.set_state(spec, ListState::Failed(why.to_owned()));
        say(&format!("FAIL:{}:{why}", spec.id));
    }

    /// The engine's answer for part `id`.
    fn result(&mut self, id: &str, status: ListStatus) {
        let Some(spec) = LISTS.iter().find(|spec| {
            self.pending
                .get(spec.id)
                .is_some_and(|p| p.parts.iter().any(|part| part == id))
        }) else {
            return;
        };
        let loading = self.pending[spec.id].loading;
        match status {
            ListStatus::Ready => {
                let Some(p) = self.pending.get_mut(spec.id) else {
                    return;
                };
                p.ready.insert(id.to_owned());
                if p.ready.len() == p.parts.len() {
                    let (rules, dropped) = (p.rules, p.dropped);
                    self.promote(spec);
                    self.blocker.update_list(spec.id, |l| {
                        l.state = ListState::Ready;
                        l.rules = rules;
                        l.dropped = dropped;
                    });
                    say(&format!("READY:{}:rules={rules}", spec.id));
                }
            }
            // The store has no such list (a new profile, a WebKit that changed its format):
            // convert the source and compile it, once.
            ListStatus::NotStored | ListStatus::Stale if loading => {
                say(&format!("RECOMPILE:{}:{}", spec.id, status.name()));
                match fs::read(self.path(spec, "txt")) {
                    Ok(source) => self.compile(spec, &source, None),
                    Err(_) => self.set_state(spec, ListState::Waiting),
                }
            }
            // Another part of the same load, answered after the first one started the compile.
            ListStatus::NotStored | ListStatus::Stale => {}
            ListStatus::Unavailable => {
                self.pending.remove(spec.id);
                self.set_state(spec, ListState::Unavailable);
                say(&format!("UNAVAILABLE:{}", spec.id));
            }
            other => {
                let (parts, trial) = self
                    .pending
                    .get(spec.id)
                    .map(|p| (p.parts.clone(), p.candidate.is_some()))
                    .unwrap_or_default();
                let why = format!("the engine did not take the list ({})", other.name());
                self.refuse(spec, &parts, &why, trial);
            }
        }
    }
}

mod fetch;

#[cfg(test)]
mod tests;

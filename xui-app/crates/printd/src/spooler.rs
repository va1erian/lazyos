//! The queue: jobs by id, their limits and owners, and the thread that sends
//! queued jobs one at a time ([`crate::send`]).
//!
//! Every call names its caller's uid, as the kernel stamped it on the
//! request: a job answers only to the uid that opened it (and to root), and
//! anyone else is told it does not exist rather than that it is not theirs.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use ipp::uri::PrinterUri;

use crate::send;
use crate::store::{Record, Store};
use crate::{
    HISTORY, JobId, JobInfo, MAX_ACTIVE, MAX_FIELD, MAX_JOB_BYTES, MAX_OPEN_PER_OWNER,
    MAX_SPOOL_BYTES, Request, State,
};

/// An open job no write has touched for this long is dropped: its app is
/// gone. Nothing of it had reached the printer.
const IDLE: Duration = Duration::from_secs(300);
/// How often the sending thread wakes with nothing to do, for the idle check.
const TICK: Duration = Duration::from_secs(1);
/// The user name a request that gave none is sent under.
const DEFAULT_USER: &str = "lazyos";

/// "Not yours" and "not there" read the same.
const NO_JOB: &str = "There is no such print job";

struct Job {
    record: Record,
    ink: String,
    /// Bytes in the document so far.
    bytes: u64,
    /// The last write, for [`IDLE`].
    touched: Instant,
    /// Set to stop the job while it is being sent.
    cancel: Arc<AtomicBool>,
}

impl Job {
    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.record.id,
            name: self.record.ticket.name.clone(),
            printer: self.record.printer.clone(),
            state: self.record.state,
            line: self.record.line.clone(),
            ink: self.ink.clone(),
        }
    }
}

/// A job the printer may still hold from an earlier run: canceled there
/// before anything else is sent.
pub(crate) struct Orphan {
    pub printer: String,
    pub user: String,
    pub printer_job: i32,
}

struct Inner {
    jobs: BTreeMap<JobId, Job>,
    next: JobId,
    orphans: Vec<Orphan>,
}

/// Told each job that ends, for a service's log.
pub type Report = Box<dyn Fn(&JobInfo) + Send + Sync>;

/// The print queue over one spool directory.
pub struct Spooler {
    store: Store,
    inner: Mutex<Inner>,
    wake: Condvar,
    report: Option<Report>,
}

/// The work the sending thread takes next.
pub(crate) enum Work {
    Orphan(Orphan),
    Job(send::Order),
}

impl Spooler {
    /// The spooler over `dir`, picking up what an earlier run left there,
    /// with its sending thread started. The thread ends once the last
    /// handle is dropped.
    pub fn open(dir: &Path) -> io::Result<Arc<Spooler>> {
        Spooler::open_reporting(dir, None)
    }

    /// [`Spooler::open`], telling `report` about every job that ends.
    pub fn open_reporting(dir: &Path, report: Option<Report>) -> io::Result<Arc<Spooler>> {
        let store = Store::open(dir)?;
        let mut inner = Inner {
            jobs: BTreeMap::new(),
            next: 1,
            orphans: Vec::new(),
        };
        for record in store.load()? {
            inner.next = inner.next.max(record.id.saturating_add(1));
            if let Some(job) = recover(&store, record, &mut inner.orphans) {
                inner.jobs.insert(job.record.id, job);
            }
        }
        let spooler = Arc::new(Spooler {
            store,
            inner: Mutex::new(inner),
            wake: Condvar::new(),
            report,
        });
        let weak = Arc::downgrade(&spooler);
        std::thread::Builder::new()
            .name("printd-send".into())
            .spawn(move || sender(weak))?;
        Ok(spooler)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens a job for `owner`.
    pub fn open_job(&self, owner: u32, request: &Request) -> Result<JobId, String> {
        let printer = PrinterUri::parse(&request.printer)?.to_ipp();
        let ticket = &request.ticket;
        let fields = [&request.user, &ticket.name, &ticket.format];
        let choices = [&ticket.media, &ticket.color_mode];
        if fields.iter().any(|f| f.len() > MAX_FIELD)
            || choices
                .iter()
                .any(|c| c.as_ref().is_some_and(|c| c.len() > MAX_FIELD))
        {
            return Err("A print job field is too long".into());
        }
        if ticket.format.is_empty() {
            return Err("The job does not say what format its document is".into());
        }
        if ticket.copies.is_some_and(|c| !(1..=99).contains(&c)) {
            return Err("Copies must be from 1 to 99".into());
        }
        if ticket.quality.is_some_and(|q| !(3..=5).contains(&q)) {
            return Err("Unknown print quality".into());
        }
        let mut inner = self.lock();
        let active = inner.jobs.values().filter(|j| !j.record.state.is_final());
        if active.clone().count() >= MAX_ACTIVE {
            return Err("The print queue is full; try again once a job is done".into());
        }
        let open = active.filter(|j| j.record.state == State::Open && j.record.owner == owner);
        if open.count() >= MAX_OPEN_PER_OWNER {
            return Err("Too many print jobs are being prepared at once".into());
        }
        let id = inner.next;
        inner.next = id
            .checked_add(1)
            .ok_or("The print queue ran out of job numbers")?;
        let user = if request.user.trim().is_empty() {
            DEFAULT_USER.to_owned()
        } else {
            request.user.clone()
        };
        let record = Record {
            id,
            owner,
            state: State::Open,
            printer,
            user,
            ticket: ticket.clone(),
            printer_job: None,
            line: "Preparing...".into(),
        };
        std::fs::File::create(self.store.document(id)).map_err(|e| spool_error(&e))?;
        self.store.save(&record).map_err(|e| spool_error(&e))?;
        inner.jobs.insert(
            id,
            Job {
                record,
                ink: String::new(),
                bytes: 0,
                touched: Instant::now(),
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
        Ok(id)
    }

    /// Appends to an open job's document.
    pub fn write(&self, owner: u32, id: JobId, bytes: &[u8]) -> Result<(), String> {
        let mut inner = self.lock();
        let spooled: u64 = inner
            .jobs
            .values()
            .filter(|j| !j.record.state.is_final())
            .map(|j| j.bytes)
            .sum();
        let job = owned(&mut inner, owner, id)?;
        if job.record.state != State::Open {
            return Err("The job is already complete".into());
        }
        let more = bytes.len() as u64;
        if job.bytes + more > MAX_JOB_BYTES || spooled + more > MAX_SPOOL_BYTES {
            return Err("The document is too large for the print queue".into());
        }
        OpenOptions::new()
            .append(true)
            .open(self.store.document(id))
            .and_then(|mut file| file.write_all(bytes))
            .map_err(|e| spool_error(&e))?;
        job.bytes += more;
        job.touched = Instant::now();
        Ok(())
    }

    /// Queues an open job for its printer.
    pub fn close(&self, owner: u32, id: JobId) -> Result<(), String> {
        let mut inner = self.lock();
        let job = owned(&mut inner, owner, id)?;
        if job.record.state != State::Open {
            return Err("The job is already complete".into());
        }
        if job.bytes == 0 {
            return Err("The job has nothing to print".into());
        }
        job.record.state = State::Queued;
        job.record.line = "Waiting for the printer...".into();
        let record = job.record.clone();
        drop(inner);
        self.save(&record);
        self.wake.notify_all();
        Ok(())
    }

    /// Stops a job: one not yet sent is dropped, one being sent is canceled
    /// at the printer by the sending thread.
    pub fn cancel(&self, owner: u32, id: JobId) -> Result<(), String> {
        let mut inner = self.lock();
        let job = owned(&mut inner, owner, id)?;
        match job.record.state {
            State::Open | State::Queued => {
                // Final under the lock, so the sending thread cannot take it.
                job.record.state = State::Canceled;
                drop(inner);
                self.finish(id, State::Canceled, "Printing canceled".into());
            }
            State::Sending | State::Printing => {
                job.cancel.store(true, Ordering::Relaxed);
                job.record.line = "Canceling...".into();
                drop(inner);
                self.wake.notify_all();
            }
            State::Done | State::Failed | State::Canceled => {}
        }
        Ok(())
    }

    /// What a job is doing.
    pub fn status(&self, owner: u32, id: JobId) -> Result<JobInfo, String> {
        Ok(owned(&mut self.lock(), owner, id)?.info())
    }

    /// The jobs `owner` may see (root sees every job), oldest first.
    pub fn jobs(&self, owner: u32) -> Vec<JobInfo> {
        self.lock()
            .jobs
            .values()
            .filter(|j| owner == 0 || j.record.owner == owner)
            .map(Job::info)
            .collect()
    }

    fn save(&self, record: &Record) {
        // A record that fails to save only costs recovery after a restart;
        // the job itself goes on.
        let _ = self.store.save(record);
    }

    /// Updates job `id` from the sending thread: `edit` changes its record
    /// and ink; the record is saved when `persist` is set.
    pub(crate) fn update(
        &self,
        id: JobId,
        persist: bool,
        edit: impl FnOnce(&mut Record, &mut String),
    ) {
        let mut inner = self.lock();
        let Some(job) = inner.jobs.get_mut(&id) else {
            return;
        };
        edit(&mut job.record, &mut job.ink);
        let record = job.record.clone();
        drop(inner);
        if persist {
            self.save(&record);
        }
    }

    /// Ends job `id` as `state`, with `line`, dropping its document.
    pub(crate) fn finish(&self, id: JobId, state: State, line: String) {
        self.update(id, true, |record, _| {
            record.state = state;
            record.line = line;
        });
        self.store.remove_document(id);
        // The record is copied out first: the report runs without the lock.
        let info = self.lock().jobs.get(&id).map(Job::info);
        if let (Some(report), Some(info)) = (&self.report, info) {
            report(&info);
        }
        self.prune();
    }

    /// The next piece of work, waiting at most [`TICK`] for one.
    fn next_work(&self) -> Option<Work> {
        let mut inner = self.lock();
        if let Some(orphan) = inner.orphans.pop() {
            return Some(Work::Orphan(orphan));
        }
        if !inner.jobs.values().any(|j| j.record.state == State::Queued) {
            inner = self
                .wake
                .wait_timeout(inner, TICK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let job = inner
            .jobs
            .values_mut()
            .find(|j| j.record.state == State::Queued)?;
        job.record.state = State::Sending;
        job.record.line = "Connecting to the printer...".into();
        Some(Work::Job(send::Order {
            id: job.record.id,
            printer: job.record.printer.clone(),
            user: job.record.user.clone(),
            ticket: job.record.ticket.clone(),
            document: self.store.document(job.record.id),
            cancel: Arc::clone(&job.cancel),
        }))
    }

    /// Drops open jobs whose app went away.
    fn expire_idle(&self) {
        let now = Instant::now();
        let idle: Vec<JobId> = self
            .lock()
            .jobs
            .values()
            .filter(|j| j.record.state == State::Open && now - j.touched > IDLE)
            .map(|j| j.record.id)
            .collect();
        for id in idle {
            self.finish(
                id,
                State::Canceled,
                "The app stopped sending the document".into(),
            );
        }
    }

    /// Forgets the oldest finished jobs beyond [`HISTORY`].
    fn prune(&self) {
        let mut inner = self.lock();
        let finished: Vec<JobId> = inner
            .jobs
            .values()
            .filter(|j| j.record.state.is_final())
            .map(|j| j.record.id)
            .collect();
        let excess = finished.len().saturating_sub(HISTORY);
        for id in &finished[..excess] {
            inner.jobs.remove(id);
            self.store.remove(*id);
        }
    }
}

fn owned(inner: &mut Inner, owner: u32, id: JobId) -> Result<&mut Job, String> {
    inner
        .jobs
        .get_mut(&id)
        .filter(|j| owner == 0 || j.record.owner == owner)
        .ok_or_else(|| NO_JOB.to_owned())
}

fn spool_error(error: &io::Error) -> String {
    format!("The print queue could not store the document: {error}")
}

/// What an earlier run's record means now. A job never closed lost its app
/// with that run and is dropped; a queued one waits again; one the printer
/// was being sent is canceled there, since its document stopped arriving
/// and the printer would otherwise print what it got; one fully sent is
/// left to the printer.
fn recover(store: &Store, mut record: Record, orphans: &mut Vec<Orphan>) -> Option<Job> {
    match (record.state, record.printer_job) {
        (State::Open, _) => {
            store.remove(record.id);
            return None;
        }
        (State::Sending, None) => {
            // Create-Job never answered: nothing is known to the printer.
            record.state = State::Queued;
            record.line = "Waiting for the printer...".into();
        }
        (State::Sending, Some(printer_job)) => {
            orphans.push(Orphan {
                printer: record.printer.clone(),
                user: record.user.clone(),
                printer_job,
            });
            store.remove_document(record.id);
            record.state = State::Failed;
            record.line = "The print service restarted while sending; the job was canceled".into();
        }
        (State::Printing, _) => {
            record.state = State::Done;
            record.line = "Sent to the printer".into();
        }
        (State::Queued | State::Done | State::Failed | State::Canceled, _) => {}
    }
    let _ = store.save(&record);
    let bytes = std::fs::metadata(store.document(record.id)).map_or(0, |m| m.len());
    Some(Job {
        record,
        ink: String::new(),
        bytes,
        touched: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
    })
}

/// The sending thread: orphans first, then queued jobs in order.
fn sender(spooler: Weak<Spooler>) {
    loop {
        let Some(spooler) = spooler.upgrade() else {
            return;
        };
        spooler.expire_idle();
        match spooler.next_work() {
            Some(Work::Orphan(orphan)) => send::cancel_orphan(&orphan),
            Some(Work::Job(order)) => {
                let (state, line) = send::run(&spooler, &order);
                spooler.finish(order.id, state, line);
            }
            None => {}
        }
    }
}

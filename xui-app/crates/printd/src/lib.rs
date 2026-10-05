#![forbid(unsafe_code)]

//! The LazyOS print spooler (docs/printing-plan.md, P6).
//!
//! An app opens a job, writes the whole document into it and closes it. Only
//! then is the job queued, and only a queued job is sent to its printer, so
//! a printer never receives half a request because an app quit or crashed
//! mid-way. Each job is sent as Create-Job, then Send-Document, so the
//! printer names the job before any page leaves and every abort (cancel, a
//! dropped connection, the spooler restarting) ends in Cancel-Job instead of
//! a job the printer waits on, or prints anyway after its
//! `multiple-operation-time-out`.
//!
//! * [`Spooler`]: the queue, its spool directory and the thread that sends.
//! * [`Queue`]: what an app needs from a spooler, served in process by
//!   [`Local`] and over Messenger (`os.lazy.print.v1`) by the `xui-printd`
//!   binary.
//! * A job is two files in the spool directory: `<id>.job` (its ticket and
//!   state, `store.rs`) and `<id>.doc` (the document, deleted once sent).

#[cfg(feature = "fake")]
pub mod fake;
mod send;
mod spooler;
mod store;

use std::sync::Arc;

pub use ipp::request::Ticket;
pub use spooler::{Report, Spooler};

/// A job's number, unique in its spooler.
pub type JobId = u32;

/// Largest document one job may hold.
pub const MAX_JOB_BYTES: u64 = 192 * 1024 * 1024;
/// Largest total of the documents spooled at once.
pub const MAX_SPOOL_BYTES: u64 = 256 * 1024 * 1024;
/// Most jobs one user may have open (being written) at once.
pub const MAX_OPEN_PER_OWNER: usize = 4;
/// Most jobs not yet finished (open, queued or being sent).
pub const MAX_ACTIVE: usize = 32;
/// Finished jobs kept for [`Spooler::jobs`] before the oldest is forgotten.
pub const HISTORY: usize = 16;
/// Longest text field (printer address, user, job name, choices) accepted.
pub const MAX_FIELD: usize = 255;

/// Where a job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Being written by its app; nothing has gone to the printer.
    Open,
    /// Complete and waiting its turn.
    Queued,
    /// The printer has the job and the document is going to it.
    Sending,
    /// The printer has the whole document and is printing it.
    Printing,
    /// The printer finished it.
    Done,
    /// It ended on an error; [`JobInfo::line`] says which.
    Failed,
    /// Canceled by its owner (or at the printer).
    Canceled,
}

impl State {
    /// Whether the job is over.
    pub fn is_final(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Canceled)
    }

    /// The word the job file stores.
    pub fn word(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Queued => "queued",
            State::Sending => "sending",
            State::Printing => "printing",
            State::Done => "done",
            State::Failed => "failed",
            State::Canceled => "canceled",
        }
    }

    /// The state a stored word names.
    pub fn from_word(word: &str) -> Option<State> {
        [
            State::Open,
            State::Queued,
            State::Sending,
            State::Printing,
            State::Done,
            State::Failed,
            State::Canceled,
        ]
        .into_iter()
        .find(|state| state.word() == word)
    }
}

/// What a job is doing, for the app that printed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub id: JobId,
    /// The job's name (the document's title).
    pub name: String,
    /// The printer's URI, `ipp://host:631/ipp/print`.
    pub printer: String,
    pub state: State,
    /// One line for the user, in the printer's words when it gave any.
    pub line: String,
    /// Ink levels, `tri-color 90%, black 50%`, once known.
    pub ink: String,
}

/// A job to open: where it goes, who asks and the print dialog's choices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// The printer's address as typed (see [`ipp::uri::PrinterUri::parse`]).
    pub printer: String,
    /// `requesting-user-name`: a label the printer shows, not an identity.
    pub user: String,
    pub ticket: Ticket,
}

impl Request {
    /// Whether every text field is at most [`MAX_FIELD`] bytes, as the
    /// spooler requires.
    pub fn fields_fit(&self) -> bool {
        let ticket = &self.ticket;
        [&self.printer, &self.user, &ticket.name, &ticket.format]
            .iter()
            .all(|f| f.len() <= MAX_FIELD)
            && [&ticket.media, &ticket.color_mode]
                .iter()
                .all(|c| c.as_ref().is_none_or(|c| c.len() <= MAX_FIELD))
    }
}

/// What an app needs from a print spooler. Errors are one line for the
/// user.
pub trait Queue {
    /// Opens a job; the document follows through [`Queue::write`].
    fn open(&self, request: &Request) -> Result<JobId, String>;
    /// Appends `bytes` to an open job's document.
    fn write(&self, job: JobId, bytes: &[u8]) -> Result<(), String>;
    /// Says the document is complete: the job is queued for its printer.
    fn close(&self, job: JobId) -> Result<(), String>;
    /// Stops a job wherever it is; a job the printer has is canceled there.
    fn cancel(&self, job: JobId) -> Result<(), String>;
    /// What the job is doing now.
    fn status(&self, job: JobId) -> Result<JobInfo, String>;
}

/// A [`Spooler`] in this process, used as `owner`: LazyWriter's host runs
/// and tests print through it.
#[derive(Clone)]
pub struct Local {
    pub spooler: Arc<Spooler>,
    pub owner: u32,
}

impl Queue for Local {
    fn open(&self, request: &Request) -> Result<JobId, String> {
        self.spooler.open_job(self.owner, request)
    }

    fn write(&self, job: JobId, bytes: &[u8]) -> Result<(), String> {
        self.spooler.write(self.owner, job, bytes)
    }

    fn close(&self, job: JobId) -> Result<(), String> {
        self.spooler.close(self.owner, job)
    }

    fn cancel(&self, job: JobId) -> Result<(), String> {
        self.spooler.cancel(self.owner, job)
    }

    fn status(&self, job: JobId) -> Result<JobInfo, String> {
        self.spooler.status(self.owner, job)
    }
}

#![forbid(unsafe_code)]

//! The platform seam: what LazyWriter needs from the system it runs on.
//!
//! The LazyOS binary fills it with the atomic writer from `xui_app`'s
//! platform layer, `$HOME` (or `/transient`) as the start folder and the
//! families it registered with the shaper and the `printd` client; tests
//! fill it with plain `std::fs`, a temp folder and a spooler in process.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use printd::{JobId, JobInfo, Local, Queue, Request, Spooler};
use xui_core::widget::{FileSystem, StdFileSystem};

/// Writes a whole file, replacing what was there.
pub type WriteFn = dyn Fn(&Path, &[u8]) -> Result<(), String>;

/// What the app takes from its platform.
#[derive(Clone)]
pub struct Host {
    /// Writes saved documents, exported Markdown and exported pictures. On
    /// LazyOS it writes a temp file and renames it, refusing symlinks.
    pub write: Rc<WriteFn>,
    /// Where the pickers open for an untitled document.
    pub start_dir: PathBuf,
    /// What the pickers browse.
    pub file_system: Rc<dyn FileSystem>,
    /// The family the Serif choice names, as the font file declares it.
    pub serif_family: String,
    /// The family the Mono choice names.
    pub mono_family: String,
    /// The printer the last successful job went to, for the print bar.
    pub last_printer: Rc<dyn Fn() -> Option<String>>,
    /// Remembers a printer that took a job.
    pub remember_printer: Rc<dyn Fn(&str)>,
    /// The print spooler jobs go to: `printd` over Messenger on LazyOS.
    pub print_queue: Rc<dyn Queue>,
}

impl Host {
    /// A host over `std::fs` with plain (non-atomic) writes, no memory of
    /// printers and a print spooler of its own in this process (spooling in
    /// the temp folder), for tests and host runs.
    pub fn std(start_dir: impl Into<PathBuf>) -> Host {
        Host {
            write: Rc::new(|path, bytes| std::fs::write(path, bytes).map_err(|e| e.to_string())),
            start_dir: start_dir.into(),
            file_system: Rc::new(StdFileSystem),
            serif_family: "serif".to_owned(),
            mono_family: "monospace".to_owned(),
            last_printer: Rc::new(|| None),
            remember_printer: Rc::new(|_| {}),
            print_queue: local_queue(),
        }
    }
}

/// A spooler in this process, started on the first job (so a host that
/// replaces it never starts one), spooling in the temp folder. One that
/// cannot start refuses every job with the reason, so printing fails with it
/// instead of the app failing to start.
fn local_queue() -> Rc<dyn Queue> {
    Rc::new(OnDemand::default())
}

#[derive(Default)]
struct OnDemand(OnceCell<Result<Local, String>>);

impl OnDemand {
    fn queue(&self) -> Result<&Local, String> {
        self.0
            .get_or_init(|| {
                // One folder per spooler: two on one folder would each take
                // the other's open jobs for a dead run's.
                static NEXT: AtomicU32 = AtomicU32::new(0);
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir()
                    .join(format!("lazywriter-spool-{}-{n}", std::process::id()));
                Spooler::open(&dir)
                    .map(|spooler| Local { spooler, owner: 0 })
                    .map_err(|e| format!("The print queue could not start: {e}"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl Queue for OnDemand {
    fn open(&self, request: &Request) -> Result<JobId, String> {
        self.queue()?.open(request)
    }

    fn write(&self, job: JobId, bytes: &[u8]) -> Result<(), String> {
        self.queue()?.write(job, bytes)
    }

    fn close(&self, job: JobId) -> Result<(), String> {
        self.queue()?.close(job)
    }

    fn cancel(&self, job: JobId) -> Result<(), String> {
        self.queue()?.cancel(job)
    }

    fn status(&self, job: JobId) -> Result<JobInfo, String> {
        self.queue()?.status(job)
    }
}

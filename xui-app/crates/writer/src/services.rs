#![forbid(unsafe_code)]

//! What LazyWriter needs beyond the shared [`AppHost`](xui_host::AppHost)
//! (file system, start folder, atomic write, evidence): the font families
//! it registered, the printer it remembers and the print spooler.
//!
//! The LazyOS binary fills it with the families `register_writer` registered,
//! the printer kept in its settings and the `printd` client; tests fill it
//! with a spooler in process.

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use printd::{JobId, JobInfo, Local, Queue, Request, Spooler};

/// What the app takes from its platform besides the host.
#[derive(Clone)]
pub struct Services {
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

impl Services {
    /// Generic family names, no memory of printers and a print spooler of
    /// its own in this process (spooling in the temp folder), for tests and
    /// host runs.
    pub fn std() -> Services {
        Services {
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

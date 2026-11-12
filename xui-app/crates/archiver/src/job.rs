//! Long operations on a worker thread.
//!
//! A [`Job`] runs one [`Task`] on its own thread with a shared
//! [`Progress`]. The worker answers the window with messages (`xui-work`,
//! through the backend's waker): [`Msg::Progress`](crate::app::Msg) when the
//! progress moved (at most every [`PROGRESS_GAP`], for the window's bar) and
//! [`Msg::Finished`](crate::app::Msg) with the outcome. Every task that
//! changes an archive reopens it on the worker too, so the window only ever
//! swaps in a finished [`Archive`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lazyarc::extract::{self, Options, Report};
use lazyarc::{create, rewrite, Archive, Error, Format, Level, Progress, Source};
use xui_work::{Once, Sink};

/// How often a running task's progress moves the window's bar.
pub const PROGRESS_GAP: Duration = Duration::from_millis(100);

/// What to do.
#[derive(Clone, Debug)]
pub enum Task {
    /// Open and list an archive.
    Open(PathBuf),
    /// Extract the entries at or below `paths` (all when empty) into `dest`,
    /// with `strip` removed from their paths.
    Extract {
        dest: PathBuf,
        paths: Vec<String>,
        strip: String,
    },
    /// Test every entry.
    Test,
    /// Create `dest` from `sources`, then open it.
    Create {
        dest: PathBuf,
        format: Format,
        level: Level,
        sources: Vec<PathBuf>,
    },
    /// Add `sources` under archive folder `folder`, then reopen.
    Add {
        sources: Vec<PathBuf>,
        folder: String,
        level: Level,
    },
    /// Delete the entries at or below `paths`, then reopen.
    Delete { paths: Vec<String> },
    /// Extract one file into a fresh folder of `scratch` to open it.
    OpenInside { path: String, scratch: PathBuf },
}

impl Task {
    /// The verb the progress line shows.
    pub fn verb(&self) -> &'static str {
        match self {
            Task::Open(_) => "Opening",
            Task::Extract { .. } => "Extracting",
            Task::Test => "Testing",
            Task::Create { .. } => "Creating",
            Task::Add { .. } => "Adding",
            Task::Delete { .. } => "Deleting",
            Task::OpenInside { .. } => "Unpacking",
        }
    }

    /// The file or folder the task writes or reads, for a failure message.
    pub fn target(&self) -> Option<&std::path::Path> {
        match self {
            Task::Open(path) => Some(path),
            Task::Extract { dest, .. } | Task::Create { dest, .. } => Some(dest),
            _ => None,
        }
    }
}

/// What a finished task produced.
#[derive(Debug)]
pub enum Outcome {
    Opened(Archive),
    Extracted {
        report: Report,
        dest: PathBuf,
    },
    Tested(Report),
    /// A create, add or delete, with the archive reopened.
    Changed {
        archive: Archive,
        report: Report,
        verb: &'static str,
    },
    /// A file ready to be opened.
    Unpacked(PathBuf),
}

/// What a finished task left, in a message: taken once by the window.
pub type Finished = Once<Result<Outcome, Error>>;

/// A task running on its worker thread.
pub struct Job {
    pub task: Task,
    pub progress: Arc<Progress>,
}

impl Job {
    /// Start `task` against `archive` (the open one, for tasks that need it).
    /// `progress` is rung when the task's progress moves; `finished` gets the
    /// outcome, also when the worker could not start or panicked.
    pub fn start(
        task: Task,
        archive: Option<Arc<Archive>>,
        progress: Sink<()>,
        finished: Sink<Finished>,
    ) -> Job {
        let shared = Arc::new(Progress::observed(PROGRESS_GAP, move || {
            progress.ring();
        }));
        let (worker_task, worker_progress) = (task.clone(), Arc::clone(&shared));
        xui_work::spawn(
            &finished,
            "archiver-job",
            move || run(worker_task, archive.as_deref(), &worker_progress),
            |result| {
                Once::new(
                    result.unwrap_or_else(|failed| Err(Error::unsupported(failed.to_string()))),
                )
            },
        );
        Job {
            task,
            progress: shared,
        }
    }

    /// Ask the task to stop.
    pub fn cancel(&self) {
        self.progress.cancel();
    }
}

fn needs(archive: Option<&Archive>) -> Result<&Archive, Error> {
    archive.ok_or_else(|| Error::unsupported("no archive is open"))
}

/// Run `task` to completion on the calling thread.
pub fn run(
    task: Task,
    archive: Option<&Archive>,
    progress: &Arc<Progress>,
) -> Result<Outcome, Error> {
    match task {
        Task::Open(path) => Archive::open(&path, progress).map(Outcome::Opened),
        Task::Extract { dest, paths, strip } => {
            let archive = needs(archive)?;
            let options = Options {
                strip,
                ..Options::default()
            };
            let wanted =
                |entry: &lazyarc::Entry| paths.is_empty() || rewrite::under_any(entry, &paths);
            let report = extract::extract(archive, &wanted, &dest, &options, progress)?;
            Ok(Outcome::Extracted { report, dest })
        }
        Task::Test => extract::test(needs(archive)?, progress).map(Outcome::Tested),
        Task::Create {
            dest,
            format,
            level,
            sources,
        } => {
            let report =
                create::create(&dest, format, level, &Source::under("", &sources), progress)?;
            let archive = Archive::open(&dest, &Arc::new(Progress::new()))?;
            Ok(Outcome::Changed {
                archive,
                report,
                verb: "Created",
            })
        }
        Task::Add {
            sources,
            folder,
            level,
        } => {
            let archive = needs(archive)?;
            let report = rewrite::add(archive, &Source::under(&folder, &sources), level, progress)?;
            let archive = Archive::open(&archive.path, &Arc::new(Progress::new()))?;
            Ok(Outcome::Changed {
                archive,
                report,
                verb: "Added",
            })
        }
        Task::Delete { paths } => {
            let archive = needs(archive)?;
            let report = rewrite::delete(archive, &paths, progress)?;
            let archive = Archive::open(&archive.path, &Arc::new(Progress::new()))?;
            Ok(Outcome::Changed {
                archive,
                report,
                verb: "Deleted",
            })
        }
        Task::OpenInside { path, scratch } => {
            let archive = needs(archive)?;
            let strip = crate::folder::parent(&path);
            let options = Options {
                strip,
                ..Options::default()
            };
            let report =
                extract::extract(archive, &|e| e.path == path, &scratch, &options, progress)?;
            match report.top_level.first() {
                Some(file) if report.skipped.is_empty() => Ok(Outcome::Unpacked(file.clone())),
                _ => Err(Error::unsupported(
                    report
                        .skipped
                        .first()
                        .map(|(_, why)| why.clone())
                        .unwrap_or_else(|| "nothing to open".into()),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_finishes_on_its_thread() {
        let dir = std::env::temp_dir().join(format!("archiver-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        let dest = dir.join("a.zip");
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let finished = Sink::from_fn(move |done| tx.lock().unwrap().send(done).is_ok());
        let _job = Job::start(
            Task::Create {
                dest: dest.clone(),
                format: Format::Zip,
                level: Level::Normal,
                sources: vec![file],
            },
            None,
            Sink::from_fn(|()| true),
            finished,
        );
        let outcome = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .take()
            .unwrap();
        match outcome.unwrap() {
            Outcome::Changed { archive, verb, .. } => {
                assert_eq!(verb, "Created");
                assert_eq!(archive.entries[0].path, "a.txt");
            }
            other => panic!("unexpected {other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tasks_that_need_an_archive_say_so() {
        let outcome = run(Task::Test, None, &Arc::new(Progress::new()));
        assert!(matches!(outcome, Err(Error::Unsupported(_))));
    }
}

//! Long operations on a worker thread.
//!
//! A [`Job`] runs one [`Task`] on its own thread with a shared
//! [`Progress`]; the window polls [`Job::poll`] from a timer (LazyOS's
//! backend has no cross-thread waker, as Net Tools found) and reads the
//! progress for its bar. Every task that changes an archive reopens it on the
//! worker too, so the window only ever swaps in a finished [`Archive`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lazyarc::extract::{self, Options, Report};
use lazyarc::{create, rewrite, Archive, Error, Format, Level, Progress, Source};

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

type Slot = Arc<Mutex<Option<Result<Outcome, Error>>>>;

/// A task running on its worker thread.
pub struct Job {
    pub task: Task,
    pub progress: Arc<Progress>,
    slot: Slot,
}

impl Job {
    /// Start `task` against `archive` (the open one, for tasks that need it).
    pub fn start(task: Task, archive: Option<Arc<Archive>>) -> Result<Job, String> {
        let progress = Arc::new(Progress::new());
        let slot: Slot = Arc::default();
        let (worker_task, worker_progress, worker_slot) =
            (task.clone(), Arc::clone(&progress), Arc::clone(&slot));
        std::thread::Builder::new()
            .name("archiver-job".into())
            .spawn(move || {
                let outcome = run(worker_task, archive.as_deref(), &worker_progress);
                *worker_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            })
            .map_err(|error| format!("could not start a worker thread: {error}"))?;
        Ok(Job {
            task,
            progress,
            slot,
        })
    }

    /// The outcome, once the task has finished.
    pub fn poll(&self) -> Option<Result<Outcome, Error>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
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
        let job = Job::start(
            Task::Create {
                dest: dest.clone(),
                format: Format::Zip,
                level: Level::Normal,
                sources: vec![file],
            },
            None,
        )
        .unwrap();
        let outcome = loop {
            if let Some(outcome) = job.poll() {
                break outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
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

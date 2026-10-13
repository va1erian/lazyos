//! The window's downloads: the list `about:downloads` shows, and the summary
//! the status bar shows while any is running.
//!
//! The engine writes the file ([`Saver`] chose where); this side keeps the rows, renames the
//! `.part` file when the engine says every byte arrived, and tells the user with a toast.
//!
//! Serial evidence: `WEBPLUS:DOWNLOAD:START:<file>`, then
//! `WEBPLUS:DOWNLOAD:DONE:<file>:<bytes>` or `WEBPLUS:DOWNLOAD:FAIL:<reason>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

use crate::downloads::Saver;
use crate::marker_text;
use crate::pages::{self, DownloadRow, DownloadState};

/// How a toast reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Error,
}

/// One toast (`os.lazy.notify`) as this module asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub level: ToastLevel,
    pub title: String,
    pub body: String,
    /// Stays until it is replaced (a running download's).
    pub sticky: bool,
    /// The toast this one replaces in place.
    pub replaces: Option<u64>,
}

/// Where toasts go: the platform's notification service.
pub trait Toaster: Send + 'static {
    /// Shows `toast`; the id the service gave it.
    fn post(&mut self, toast: &Toast) -> Result<u64, String>;
    /// Takes a toast down.
    fn close(&mut self, id: u64);
}

/// One download this window started.
struct Transfer {
    id: u64,
    row: DownloadRow,
}

/// One toast request for the notifier thread.
enum Job {
    /// A download started: show its toast.
    Start(u64, Toast),
    /// It ended: update that toast in place (and forget it).
    End(u64, Toast),
}

/// Posts the downloads' toasts from a thread of its own: a toast is
/// best-effort and a slow shell must never delay the window.
struct Notifier {
    jobs: Sender<Job>,
}

impl Notifier {
    fn spawn(mut toaster: Box<dyn Toaster>) -> Option<Notifier> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name(String::from("notify"))
            .spawn(move || {
                // Download id -> its toast, so the end replaces the start.
                let mut toasts: HashMap<u64, u64> = HashMap::new();
                while let Ok(job) = inbox.recv() {
                    match job {
                        Job::Start(id, toast) => {
                            if let Ok(shown) = toaster.post(&toast) {
                                toasts.insert(id, shown);
                            }
                        }
                        Job::End(id, toast) => {
                            let start = toasts.remove(&id);
                            let ended = toaster.post(&Toast {
                                replaces: start,
                                ..toast
                            });
                            // The start toast is sticky: if its replacement
                            // did not go, take it down rather than leave
                            // "Downloading" on screen for a finished file.
                            if let (Err(_), Some(start)) = (ended, start) {
                                toaster.close(start);
                            }
                        }
                    }
                }
            })
            .ok()?;
        Some(Notifier { jobs })
    }
}

/// The downloads, oldest first.
pub struct Transfers {
    list: Vec<Transfer>,
    saver: Arc<Saver>,
    notifier: Option<Notifier>,
}

/// What the status bar shows about the running downloads.
pub struct Summary {
    pub text: String,
    /// Percent done, when every running download announced its size.
    pub percent: Option<i32>,
}

/// What a download that started is.
pub struct Started {
    pub id: u64,
    pub url: String,
    pub name: String,
    pub total: Option<u64>,
}

impl Transfers {
    /// The downloads of a window whose files `saver` places; with a `toaster`, each also shows a
    /// notification (`os.lazy.notify`).
    pub fn new(saver: Arc<Saver>, toaster: Option<Box<dyn Toaster>>) -> Transfers {
        Transfers {
            list: Vec::new(),
            saver,
            notifier: toaster.and_then(Notifier::spawn),
        }
    }

    /// The folder the downloads go to.
    pub fn folder(&self) -> &Path {
        self.saver.dir()
    }

    /// A download started; the status line for it. The row shows the file's name as saved
    /// (`file (1).zip`), not the one the server suggested.
    pub fn started(&mut self, info: Started) -> String {
        let name = self
            .saver
            .destination(info.id)
            .and_then(|d| d.path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or(info.name);
        crate::named_marker(&format!("DOWNLOAD:START:{}", marker_text(&name)));
        // Until it ends (then the toast is updated in place).
        if let Some(notifier) = &self.notifier {
            let _ = notifier.jobs.send(Job::Start(
                info.id,
                Toast {
                    level: ToastLevel::Info,
                    title: String::from("Downloading"),
                    body: name.clone(),
                    sticky: true,
                    replaces: None,
                },
            ));
        }
        let status = format!("Downloading {name}");
        self.list.push(Transfer {
            id: info.id,
            row: DownloadRow {
                name,
                url: info.url,
                received: 0,
                total: info.total,
                state: DownloadState::Running,
            },
        });
        status
    }

    pub fn progress(&mut self, id: u64, received: u64, total: Option<u64>) {
        if let Some(t) = self.find(id) {
            t.row.received = received;
            if total.is_some() {
                t.row.total = total;
            }
        }
    }

    /// A download ended; the status line for it. `error` is why it did not complete. A
    /// completed download is renamed from its `.part` here: a rename that fails is a failure.
    pub fn finished(&mut self, id: u64, error: Option<String>) -> Option<String> {
        let folder = self.saver.dir().display().to_string();
        let completed = error.is_none();
        let saved_as = self.saver.destination(id).map(|d| d.path);
        let error = error.or_else(|| {
            self.saver
                .finish(id, true)
                .err()
                .map(|e| format!("cannot save the file: {e}"))
        });
        if !completed {
            let _ = self.saver.finish(id, false);
        }
        let jobs = self.notifier.as_ref().map(|n| n.jobs.clone());
        let t = self.find(id)?;
        let name = t.row.name.clone();
        let toast = |level, title: &str, body: String| Toast {
            level,
            title: title.to_owned(),
            body,
            sticky: false,
            replaces: None,
        };
        let toast = match &error {
            None => toast(
                ToastLevel::Success,
                "Download complete",
                format!("{name} was saved to {folder}"),
            ),
            Some(why) => toast(
                ToastLevel::Error,
                "Download failed",
                format!("{name}: {why}"),
            ),
        };
        if let Some(jobs) = jobs {
            let _ = jobs.send(Job::End(id, toast));
        }
        Some(match error {
            None => {
                t.row.state = DownloadState::Done;
                // The engine's progress can be missing for a quick download: the file is the truth.
                if let Some(size) = saved_as
                    .and_then(|path| std::fs::metadata(path).ok())
                    .map(|m| m.len())
                {
                    t.row.received = size;
                }
                crate::named_marker(&format!(
                    "DOWNLOAD:DONE:{}:{}",
                    marker_text(&name),
                    t.row.received
                ));
                format!("Saved {name} to {folder}")
            }
            Some(why) => {
                crate::named_marker(&format!("DOWNLOAD:FAIL:{}", marker_text(&why)));
                t.row.state = DownloadState::Failed(why.clone());
                format!("Download of {name} failed: {why}")
            }
        })
    }

    fn find(&mut self, id: u64) -> Option<&mut Transfer> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    /// The rows the downloads page lists.
    pub fn rows(&self) -> Vec<DownloadRow> {
        self.list.iter().map(|t| t.row.clone()).collect()
    }

    /// Download `n` of the list, to open: only one that completed.
    pub fn saved(&self, n: usize) -> Option<PathBuf> {
        let t = self.list.get(n)?;
        (t.row.state == DownloadState::Done)
            .then(|| self.saver.destination(t.id).map(|d| d.path))
            .flatten()
    }

    /// Download `n`'s id, while it runs.
    pub fn running(&self, n: usize) -> Option<u64> {
        let t = self.list.get(n)?;
        (t.row.state == DownloadState::Running).then_some(t.id)
    }

    /// Whether any download runs.
    pub fn any_running(&self) -> bool {
        self.list
            .iter()
            .any(|t| t.row.state == DownloadState::Running)
    }

    /// What the status bar shows, or `None` when nothing is running.
    pub fn summary(&self) -> Option<Summary> {
        let running: Vec<&DownloadRow> = self
            .list
            .iter()
            .map(|t| &t.row)
            .filter(|r| r.state == DownloadState::Running)
            .collect();
        let first = running.first()?;
        let received: u64 = running.iter().map(|r| r.received).sum();
        let total: Option<u64> = running.iter().map(|r| r.total).sum();
        let text = if running.len() == 1 {
            format!("{}: {}", first.name, pages::progress(received, total))
        } else {
            format!(
                "{} downloads: {}",
                running.len(),
                pages::progress(received, total)
            )
        };
        let percent = total
            .filter(|t| *t > 0)
            .map(|t| (received.min(t) * 100 / t) as i32);
        Some(Summary { text, percent })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rig(name: &str) -> (Transfers, Arc<Saver>, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("lazywebplus-tr-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let saver = Arc::new(Saver::new(dir.clone()));
        (Transfers::new(Arc::clone(&saver), None), saver, dir)
    }

    fn start(transfers: &mut Transfers, saver: &Saver, id: u64, name: &str) -> String {
        saver.reserve(id, name).unwrap();
        transfers.started(Started {
            id,
            url: format!("http://x/{name}"),
            name: name.into(),
            total: Some(100),
        })
    }

    #[test]
    fn a_download_runs_completes_and_can_be_opened_from_its_row() {
        let (mut transfers, saver, dir) = rig("complete");
        assert!(start(&mut transfers, &saver, 7, "kit.zip").contains("kit.zip"));
        assert_eq!(transfers.running(0), Some(7));
        assert!(transfers.saved(0).is_none(), "not yet");
        transfers.progress(7, 50, Some(100));
        let summary = transfers.summary().unwrap();
        assert_eq!(summary.percent, Some(50));
        assert!(summary.text.starts_with("kit.zip:"));

        fs::write(saver.destination(7).unwrap().part, b"PK").unwrap();
        let status = transfers.finished(7, None).unwrap();
        assert!(status.starts_with("Saved kit.zip"), "{status}");
        assert_eq!(transfers.saved(0), Some(dir.join("kit.zip")));
        assert!(transfers.summary().is_none());
        assert!(!transfers.any_running());
        assert_eq!(fs::read(dir.join("kit.zip")).unwrap(), b"PK");
    }

    #[test]
    fn a_failed_download_leaves_no_file_and_no_open_link() {
        let (mut transfers, saver, dir) = rig("fail");
        start(&mut transfers, &saver, 1, "a.bin");
        fs::write(saver.destination(1).unwrap().part, b"x").unwrap();
        let status = transfers.finished(1, Some("Cancelled".into())).unwrap();
        assert!(status.contains("failed: Cancelled"));
        assert!(transfers.saved(0).is_none() && transfers.running(0).is_none());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert!(matches!(
            transfers.rows()[0].state,
            DownloadState::Failed(_)
        ));
    }

    #[test]
    fn a_completed_download_whose_file_vanished_is_a_failure_not_a_success() {
        let (mut transfers, saver, _dir) = rig("vanished");
        start(&mut transfers, &saver, 1, "a.bin");
        let status = transfers.finished(1, None).unwrap();
        assert!(status.contains("failed"), "{status}");
        assert!(transfers.saved(0).is_none());
    }

    #[test]
    fn the_row_shows_the_name_it_was_saved_under() {
        let (mut transfers, saver, _dir) = rig("names");
        start(&mut transfers, &saver, 1, "kit.zip");
        start(&mut transfers, &saver, 2, "kit.zip");
        let rows = transfers.rows();
        assert_eq!(rows[0].name, "kit.zip");
        assert_eq!(rows[1].name, "kit (1).zip");
    }

    #[test]
    fn several_running_downloads_are_summed_and_an_unknown_total_hides_the_percent() {
        let (mut transfers, saver, _dir) = rig("sum");
        start(&mut transfers, &saver, 1, "a.bin");
        saver.reserve(2, "b.bin").unwrap();
        transfers.started(Started {
            id: 2,
            url: "http://x/b".into(),
            name: "b.bin".into(),
            total: None,
        });
        transfers.progress(1, 10, None);
        transfers.progress(2, 20, None);
        let summary = transfers.summary().unwrap();
        assert!(summary.text.starts_with("2 downloads:"), "{}", summary.text);
        assert_eq!(summary.percent, None);
    }
}

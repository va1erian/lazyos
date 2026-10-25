//! The window's downloads: the list `about:downloads` shows, and the summary
//! the status bar shows while any is running.
//!
//! webd writes the file in the Downloads folder (a `.part` file renamed when every byte arrived,
//! under a name of its choosing, `lazydownloads`); this side keeps the rows and tells the user with
//! a toast.
//!
//! Serial evidence: `LAZYWEB:DOWNLOAD:START:<file>`, then
//! `LAZYWEB:DOWNLOAD:DONE:<file>:<bytes>` or `LAZYWEB:DOWNLOAD:FAIL:<reason>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

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
    /// The file, once webd said every byte is in it.
    path: Option<PathBuf>,
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
    folder: PathBuf,
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
    /// The downloads of a window whose files webd writes in `folder`; with a `toaster`, each also
    /// shows a notification (`os.lazy.notify`).
    pub fn new(folder: PathBuf, toaster: Option<Box<dyn Toaster>>) -> Transfers {
        Transfers {
            list: Vec::new(),
            folder,
            notifier: toaster.and_then(Notifier::spawn),
        }
    }

    /// The folder the downloads go to.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// A download started; the status line for it. The row shows the file's name as saved
    /// (`file (1).zip`), not the one the server suggested.
    pub fn started(&mut self, info: Started) -> String {
        let name = info.name;
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
            path: None,
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

    /// A download ended; the status line for it. `name` is the file webd wrote (`DownloadDone`),
    /// `error` why it did not complete.
    pub fn finished(
        &mut self,
        id: u64,
        name: Option<&str>,
        error: Option<String>,
    ) -> Option<String> {
        let folder = self.folder.display().to_string();
        // Only a plain file name of the folder: webd names it, but a name is not a path.
        let saved_as = name
            .filter(|name| Path::new(name).file_name().and_then(|n| n.to_str()) == Some(*name))
            .map(|name| self.folder.join(name));
        let error = match (error, &saved_as) {
            (Some(why), _) => Some(why),
            (None, None) => Some(String::from("the engine named no file")),
            (None, Some(_)) => None,
        };
        let jobs = self.notifier.as_ref().map(|n| n.jobs.clone());
        let t = self.find(id)?;
        if let (None, Some(path)) = (&error, saved_as) {
            if let Some(file) = path.file_name() {
                t.row.name = file.to_string_lossy().into_owned();
            }
            t.path = Some(path);
        }
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
                // The engine's progress can be missing for a quick download: the file is the truth
                // when it can be read.
                if let Some(size) = t
                    .path
                    .as_ref()
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
            .then(|| t.path.clone())
            .flatten()
    }

    /// Download `n`'s id, while it runs.
    pub fn running(&self, n: usize) -> Option<u64> {
        let t = self.list.get(n)?;
        (t.row.state == DownloadState::Running).then_some(t.id)
    }

    /// The ids of the downloads that run.
    pub fn running_ids(&self) -> Vec<u64> {
        self.list
            .iter()
            .filter(|t| t.row.state == DownloadState::Running)
            .map(|t| t.id)
            .collect()
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

    fn rig(name: &str) -> (Transfers, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lazyweb-tr-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        (Transfers::new(dir.clone(), None), dir)
    }

    fn start(transfers: &mut Transfers, id: u64, name: &str) -> String {
        transfers.started(Started {
            id,
            url: format!("http://x/{name}"),
            name: name.into(),
            total: Some(100),
        })
    }

    #[test]
    fn a_download_runs_completes_and_can_be_opened_from_its_row() {
        let (mut transfers, dir) = rig("complete");
        assert!(start(&mut transfers, 7, "kit.zip").contains("kit.zip"));
        assert_eq!(transfers.running(0), Some(7));
        assert!(transfers.saved(0).is_none(), "not yet");
        transfers.progress(7, 50, Some(100));
        let summary = transfers.summary().unwrap();
        assert_eq!(summary.percent, Some(50));
        assert!(summary.text.starts_with("kit.zip:"));

        fs::write(dir.join("kit.zip"), b"PK").unwrap();
        let status = transfers.finished(7, Some("kit.zip"), None).unwrap();
        assert!(status.starts_with("Saved kit.zip"), "{status}");
        assert_eq!(transfers.saved(0), Some(dir.join("kit.zip")));
        assert_eq!(transfers.rows()[0].received, 2, "the file's size");
        assert!(transfers.summary().is_none());
        assert!(!transfers.any_running());
    }

    #[test]
    fn a_failed_download_has_no_open_link() {
        let (mut transfers, _dir) = rig("fail");
        start(&mut transfers, 1, "a.bin");
        let status = transfers
            .finished(1, None, Some("Cancelled".into()))
            .unwrap();
        assert!(status.contains("failed: Cancelled"));
        assert!(transfers.saved(0).is_none() && transfers.running(0).is_none());
        assert!(matches!(
            transfers.rows()[0].state,
            DownloadState::Failed(_)
        ));
    }

    #[test]
    fn a_done_name_that_is_a_path_is_not_a_file_of_the_folder() {
        let (mut transfers, _dir) = rig("path");
        start(&mut transfers, 1, "a.bin");
        let status = transfers.finished(1, Some("../a.bin"), None).unwrap();
        assert!(status.contains("failed"), "{status}");
        assert!(transfers.saved(0).is_none());
    }

    #[test]
    fn the_row_shows_the_name_webd_saved_it_under() {
        let (mut transfers, dir) = rig("names");
        start(&mut transfers, 1, "kit.zip");
        fs::write(dir.join("kit (1).zip"), b"x").unwrap();
        transfers.finished(1, Some("kit (1).zip"), None);
        assert_eq!(transfers.rows()[0].name, "kit (1).zip");
        assert_eq!(transfers.saved(0), Some(dir.join("kit (1).zip")));
    }

    #[test]
    fn several_running_downloads_are_summed_and_an_unknown_total_hides_the_percent() {
        let (mut transfers, _dir) = rig("sum");
        start(&mut transfers, 1, "a.bin");
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

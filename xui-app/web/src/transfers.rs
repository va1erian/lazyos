//! The window's downloads: the list `about:downloads` shows, and the summary
//! the status bar shows while any is running.
//!
//! Serial evidence: `WEB:DOWNLOAD:START:<file>`, then
//! `WEB:DOWNLOAD:DONE:<file>:<bytes>` or `WEB:DOWNLOAD:FAIL:<reason>`.

use std::path::{Path, PathBuf};

use lazyweb::downloads::Destinations;
use lazyweb::marker_text;
use lazyweb::pages::{self, DownloadRow, DownloadState};
use xui_netsurf::{DownloadId, DownloadInfo};

/// One download this window started.
struct Transfer {
    id: DownloadId,
    path: Option<PathBuf>,
    row: DownloadRow,
}

/// The downloads, oldest first.
pub struct Transfers {
    list: Vec<Transfer>,
    destinations: Destinations,
    folder: PathBuf,
}

/// What the status bar shows about the running downloads.
pub struct Summary {
    pub text: String,
    /// Percent done, when every running download announced its size.
    pub percent: Option<i32>,
}

impl Transfers {
    pub fn new(destinations: Destinations, folder: PathBuf) -> Transfers {
        Transfers {
            list: Vec::new(),
            destinations,
            folder,
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// A download started; the status line for it.
    pub fn started(&mut self, info: DownloadInfo) -> String {
        let path = self
            .destinations
            .lock()
            .ok()
            .and_then(|map| map.get(&info.id).cloned());
        let name = path.as_deref().and_then(Path::file_name).map_or_else(
            || info.filename.clone(),
            |n| n.to_string_lossy().into_owned(),
        );
        println!("WEB:DOWNLOAD:START:{}", marker_text(&name));
        let status = format!("Downloading {name}");
        self.list.push(Transfer {
            id: info.id,
            path,
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

    pub fn progress(&mut self, id: DownloadId, received: u64) {
        if let Some(t) = self.find(id) {
            t.row.received = received;
        }
    }

    /// A download ended; the status line for it.
    pub fn finished(&mut self, id: DownloadId, error: Option<String>) -> Option<String> {
        let t = self.find(id)?;
        let name = t.row.name.clone();
        Some(match error {
            None => {
                t.row.state = DownloadState::Done;
                println!(
                    "WEB:DOWNLOAD:DONE:{}:{}",
                    marker_text(&name),
                    t.row.received
                );
                format!("Saved {name} to {}", self.folder.display())
            }
            Some(why) => {
                println!("WEB:DOWNLOAD:FAIL:{}", marker_text(&why));
                t.row.state = DownloadState::Failed(why.clone());
                format!("Download of {name} failed: {why}")
            }
        })
    }

    fn find(&mut self, id: DownloadId) -> Option<&mut Transfer> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    /// The rows the downloads page lists.
    pub fn rows(&self) -> Vec<DownloadRow> {
        self.list.iter().map(|t| t.row.clone()).collect()
    }

    /// Download `n` of the list, to open: only one that completed.
    pub fn saved(&self, n: usize) -> Option<&Path> {
        let t = self.list.get(n)?;
        (t.row.state == DownloadState::Done)
            .then_some(t.path.as_deref())
            .flatten()
    }

    /// Download `n`'s id, while it runs.
    pub fn running(&self, n: usize) -> Option<DownloadId> {
        let t = self.list.get(n)?;
        (t.row.state == DownloadState::Running).then_some(t.id)
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

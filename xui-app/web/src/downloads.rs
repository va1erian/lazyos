//! Downloads: what the view cannot show goes to the user's Downloads folder.
//!
//! [`Saver`] is the engine's [`Downloader`]: it names each download from the
//! server's file name (sanitised, never trusted as a path), picks a name that
//! is not taken (`file (1).zip`), and writes to `<name>.part`, renamed once
//! every byte arrived and removed when the download fails or is cancelled.
//! It records where each download goes, so the window can say so and open it.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use xui_blitz::{DownloadId, DownloadInfo, DownloadSink, Downloader};

/// The longest file name kept, in bytes.
const MAX_NAME: usize = 120;

/// The name used when the server's gives nothing usable.
const FALLBACK_NAME: &str = "download";

/// Where each download is saved, by id: shared by the engine thread's
/// [`Saver`] and the window.
pub type Destinations = Arc<Mutex<HashMap<DownloadId, PathBuf>>>;

/// The engine's [`Downloader`]: saves into one folder.
pub struct Saver {
    dir: PathBuf,
    destinations: Destinations,
}

impl Saver {
    /// A saver into `dir` (created on the first download).
    pub fn new(dir: PathBuf) -> Saver {
        Saver {
            dir,
            destinations: Destinations::default(),
        }
    }

    /// Where the downloads go, for the window.
    pub fn destinations(&self) -> Destinations {
        Arc::clone(&self.destinations)
    }

    fn create(&self, info: &DownloadInfo) -> io::Result<FileSink> {
        fs::create_dir_all(&self.dir)?;
        let path = unique_path(&self.dir, &sanitize(&info.filename));
        let part = part_path(&path);
        let file = File::create(&part)?;
        Ok(FileSink { file, part, path })
    }
}

impl Downloader for Saver {
    fn start(&self, info: &DownloadInfo) -> Option<Box<dyn DownloadSink>> {
        match self.create(info) {
            Ok(sink) => {
                self.destinations
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(info.id, sink.path.clone());
                Some(Box::new(sink))
            }
            Err(e) => {
                eprintln!("lazyweb: cannot save {}: {e}", info.url);
                None
            }
        }
    }
}

/// One download's file.
struct FileSink {
    file: File,
    part: PathBuf,
    path: PathBuf,
}

impl DownloadSink for FileSink {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn finish(self: Box<Self>, result: Result<(), String>) {
        let FileSink { file, part, path } = *self;
        let kept = result.is_ok() && file.sync_all().is_ok();
        drop(file);
        if !kept || fs::rename(&part, &path).is_err() {
            let _ = fs::remove_file(&part);
        }
    }
}

/// A file name fit for the Downloads folder from what a server sent: the
/// last path segment, without control characters, separators or a leading
/// dot, bounded in length. Never empty.
pub fn sanitize(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| match c {
            c if c.is_control() => '_',
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut out = String::new();
    for c in trimmed.chars() {
        if out.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        FALLBACK_NAME.to_string()
    } else {
        out
    }
}

/// `dir/name`, or `dir/stem (n).ext` with the first `n` free (counting a
/// download still in progress as taken).
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let taken = |p: &Path| p.exists() || part_path(p).exists();
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !taken(p))
        .expect("an unbounded range finds a free name")
}

/// Where a download is written until it completes.
fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lazyweb-dl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn info(id: u64, filename: &str) -> DownloadInfo {
        DownloadInfo {
            id: DownloadId(id),
            url: format!("http://x/{filename}"),
            filename: filename.to_string(),
            mime: "application/zip".to_string(),
            total: None,
        }
    }

    #[test]
    fn server_names_cannot_escape_the_folder() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("a\nb:c?.zip"), "a_b_c_.zip");
        assert_eq!(sanitize("  "), "download");
        assert_eq!(sanitize(".."), "download");
        assert_eq!(sanitize(&"é".repeat(200)).len(), MAX_NAME);
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = temp("unique");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a.zip"));
        fs::write(dir.join("a.zip"), b"").unwrap();
        fs::write(dir.join("a (1).zip.part"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a (2).zip"));
        fs::write(dir.join("README"), b"").unwrap();
        assert_eq!(unique_path(&dir, "README"), dir.join("README (1)"));
    }

    #[test]
    fn a_completed_download_is_renamed_and_a_failed_one_removed() {
        let dir = temp("sink");
        let saver = Saver::new(dir.clone());
        let mut done = saver.start(&info(1, "files.zip")).unwrap();
        done.write(b"PK").unwrap();
        assert!(dir.join("files.zip.part").exists());
        done.finish(Ok(()));
        assert_eq!(fs::read(dir.join("files.zip")).unwrap(), b"PK");
        assert!(!dir.join("files.zip.part").exists());

        let mut failed = saver.start(&info(2, "files.zip")).unwrap();
        failed.write(b"P").unwrap();
        failed.finish(Err("Cancelled".into()));
        assert!(!dir.join("files (1).zip").exists());
        assert!(!dir.join("files (1).zip.part").exists());

        let saved = saver.destinations();
        let saved = saved.lock().unwrap();
        assert_eq!(saved[&DownloadId(1)], dir.join("files.zip"));
        assert_eq!(saved[&DownloadId(2)], dir.join("files (1).zip"));
    }
}

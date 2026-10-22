//! The browsing history: every page visited, kept per user.
//!
//! Distinct from [`crate::history`], the Back/Forward list of one window:
//! this is what the History menu and the `about:history` page show, and what
//! survives a restart. It is a text file in the app's own folder
//! (`$HOME/.apps/os.lazy.lazywebplus/history.tsv`), one visit per line,
//! `<unix seconds>\t<url>\t<title>`, appended as pages load. Loading keeps the
//! newest [`MAX_VISITS`]; the file is rewritten that short once it holds
//! twice as many lines, so it never grows without bound.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// How many visits are kept.
pub const MAX_VISITS: usize = 1000;
/// The longest title and address kept (a page may send a title of any length, and the network process,
/// which may write the profile folder, could write this file): longer ones are cut, or not kept.
pub const MAX_TITLE: usize = 512;
pub const MAX_URL: usize = 2048;
/// How much of the file is read: the newest part of it.
const MAX_READ: u64 = 2 * 1024 * 1024;

/// One visited page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub url: String,
    pub title: String,
}

/// The visits, oldest first, and the file they are kept in (none for a
/// history that is not saved).
#[derive(Debug, Default)]
pub struct Visits {
    entries: Vec<Visit>,
    file: Option<PathBuf>,
    /// Lines in the file, including the ones loading dropped.
    lines: usize,
}

/// The text of the newest [`MAX_READ`] bytes of `file` (nothing when it cannot be read); a cut-off first
/// line is dropped by the line parser.
fn read_tail(file: &Path) -> String {
    let Ok(mut f) = fs::File::open(file) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    if len > MAX_READ && f.seek(SeekFrom::Start(len - MAX_READ)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if f.take(MAX_READ).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Visits {
    /// A history kept only in memory.
    pub fn in_memory() -> Visits {
        Visits::default()
    }

    /// The history saved in `file` (created on the first visit). A missing
    /// or unreadable file is an empty history; broken lines are skipped.
    pub fn open(file: PathBuf) -> Visits {
        let text = read_tail(&file);
        let mut entries: Vec<Visit> = text.lines().filter_map(parse).collect();
        let lines = text.lines().count();
        let drop = entries.len().saturating_sub(MAX_VISITS);
        entries.drain(..drop);
        Visits {
            entries,
            file: Some(file),
            lines,
        }
    }

    /// The visits, oldest first.
    pub fn entries(&self) -> &[Visit] {
        &self.entries
    }

    /// The newest `n` distinct pages, newest first.
    pub fn recent(&self, n: usize) -> Vec<&Visit> {
        let mut seen = Vec::new();
        for visit in self.entries.iter().rev() {
            if seen.len() == n {
                break;
            }
            if !seen.iter().any(|v: &&Visit| v.url == visit.url) {
                seen.push(visit);
            }
        }
        seen
    }

    /// Records a visit to `url` at `time`. Visiting the page that is already
    /// the newest (a reload, or its title arriving) updates that entry
    /// instead. Pages that are not on the web (`about:`, `data:`) are not
    /// kept.
    pub fn record(&mut self, url: &str, title: &str, time: u64) -> io::Result<()> {
        if !worth_keeping(url) {
            return Ok(());
        }
        if url.len() > MAX_URL {
            return Ok(());
        }
        let visit = Visit {
            time,
            url: url.to_string(),
            title: one_line(title).chars().take(MAX_TITLE).collect(),
        };
        match self.entries.last_mut() {
            Some(last) if last.url == visit.url => {
                if last.title == visit.title || visit.title.is_empty() {
                    return Ok(());
                }
                *last = visit;
            }
            _ => self.entries.push(visit),
        }
        let drop = self.entries.len().saturating_sub(MAX_VISITS);
        self.entries.drain(..drop);
        self.save_last()
    }

    /// Forgets the newest visit when it is to `url`, a load that turned out
    /// to be a download.
    pub fn forget_newest(&mut self, url: &str) -> io::Result<()> {
        if self.entries.last().is_none_or(|last| last.url != url) {
            return Ok(());
        }
        self.entries.pop();
        self.rewrite()
    }

    /// Forgets every visit to `url` (a suggestion the person removed), on disk too: the file is
    /// rewritten through a temporary one, so a crash leaves the old history whole. Returns how many
    /// visits went.
    pub fn remove_url(&mut self, url: &str) -> io::Result<usize> {
        let before = self.entries.len();
        self.entries.retain(|visit| visit.url != url);
        let removed = before - self.entries.len();
        if removed > 0 {
            self.rewrite()?;
        }
        Ok(removed)
    }

    /// Forgets every visit, on disk too.
    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        self.rewrite()
    }

    /// Appends the newest visit, or rewrites the file once it has grown
    /// twice as long as the history.
    fn save_last(&mut self) -> io::Result<()> {
        let (Some(file), Some(last)) = (&self.file, self.entries.last()) else {
            return Ok(());
        };
        if self.lines + 1 >= 2 * MAX_VISITS {
            return self.rewrite();
        }
        ensure_parent(file)?;
        let mut out = OpenOptions::new().create(true).append(true).open(file)?;
        out.write_all(line(last).as_bytes())?;
        self.lines += 1;
        Ok(())
    }

    /// Writes the whole history to the file, through a temporary file so a
    /// crash leaves the old one.
    fn rewrite(&mut self) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        ensure_parent(file)?;
        let text: String = self.entries.iter().map(line).collect();
        let tmp = file.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, file)?;
        self.lines = self.entries.len();
        Ok(())
    }
}

fn ensure_parent(file: &Path) -> io::Result<()> {
    match file.parent() {
        Some(dir) => fs::create_dir_all(dir),
        None => Ok(()),
    }
}

/// Whether `url` belongs in the history: a page on the web or a file.
fn worth_keeping(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file:")
}

/// `text` without tabs or line breaks, which separate the file's fields.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn line(visit: &Visit) -> String {
    format!(
        "{}\t{}\t{}\n",
        visit.time,
        one_line(&visit.url),
        visit.title
    )
}

fn parse(line: &str) -> Option<Visit> {
    let mut fields = line.splitn(3, '\t');
    let time = fields.next()?.parse().ok()?;
    let url = fields.next()?.to_string();
    let title: String = fields
        .next()
        .unwrap_or("")
        .chars()
        .take(MAX_TITLE)
        .collect();
    (url.len() <= MAX_URL && worth_keeping(&url)).then_some(Visit { time, url, title })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazywebplus-visits-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sub").join("history.tsv")
    }

    #[test]
    fn visits_survive_a_restart() {
        let file = temp("restart");
        let mut visits = Visits::open(file.clone());
        visits.record("http://a.test/", "A", 10).unwrap();
        visits.record("http://b.test/", "B\tbee\n", 20).unwrap();
        let again = Visits::open(file);
        assert_eq!(
            again.entries(),
            &[
                Visit {
                    time: 10,
                    url: "http://a.test/".into(),
                    title: "A".into()
                },
                Visit {
                    time: 20,
                    url: "http://b.test/".into(),
                    title: "B bee".into()
                },
            ]
        );
    }

    #[test]
    fn a_removed_page_is_gone_from_memory_and_from_the_file_with_all_its_visits() {
        let file = temp("remove");
        let mut visits = Visits::open(file.clone());
        visits.record("http://a.test/", "A", 10).unwrap();
        visits.record("http://b.test/", "B", 20).unwrap();
        visits.record("http://a.test/", "A", 30).unwrap();
        assert_eq!(visits.remove_url("http://a.test/").unwrap(), 2);
        assert_eq!(visits.remove_url("http://a.test/").unwrap(), 0);
        let again = Visits::open(file.clone());
        let urls: Vec<_> = again.entries().iter().map(|v| v.url.as_str()).collect();
        assert_eq!(urls, ["http://b.test/"]);
        assert!(
            !file.with_extension("tmp").exists(),
            "the temporary file is renamed away"
        );
        // The file keeps taking visits afterwards.
        visits.record("http://c.test/", "C", 40).unwrap();
        assert_eq!(Visits::open(file).entries().len(), 2);
    }

    #[test]
    fn a_download_is_forgotten_on_disk_too() {
        let file = temp("download");
        let mut visits = Visits::open(file.clone());
        visits.record("http://a.test/", "A", 1).unwrap();
        visits.record("http://a.test/kit.zip", "A", 2).unwrap();
        visits.forget_newest("http://b.test/").unwrap();
        assert_eq!(visits.entries().len(), 2);
        visits.forget_newest("http://a.test/kit.zip").unwrap();
        let again = Visits::open(file);
        assert_eq!(again.entries().len(), 1);
        assert_eq!(again.entries()[0].url, "http://a.test/");
    }

    #[test]
    fn titles_and_addresses_are_bounded_in_memory_and_on_disk() {
        let file = temp("limits-titles");
        let mut visits = Visits::open(file.clone());
        visits
            .record("https://a.example/", &"t".repeat(MAX_TITLE * 4), 1)
            .unwrap();
        visits
            .record(
                &format!("https://b.example/{}", "x".repeat(MAX_URL)),
                "long",
                2,
            )
            .unwrap();
        assert_eq!(visits.entries().len(), 1);
        assert_eq!(visits.entries()[0].title.chars().count(), MAX_TITLE);
        // A file somebody else wrote is bounded the same way when it is read.
        fs::write(
            &file,
            format!(
                "3\thttps://c.example/\t{}\n4\thttps://d.example/{}\ttitle\n",
                "y".repeat(100_000),
                "z".repeat(MAX_URL)
            ),
        )
        .unwrap();
        let again = Visits::open(file);
        assert_eq!(again.entries().len(), 1);
        assert_eq!(again.entries()[0].title.chars().count(), MAX_TITLE);
    }

    #[test]
    fn only_the_newest_part_of_a_huge_file_is_read() {
        let file = temp("limits-huge");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut text = String::new();
        while text.len() < (MAX_READ as usize) + 100_000 {
            text.push_str("1\thttps://old.example/\told\n");
        }
        text.push_str("2\thttps://new.example/\tnew\n");
        fs::write(&file, text).unwrap();
        let visits = Visits::open(file);
        assert!(visits.entries().len() <= MAX_VISITS);
        assert_eq!(visits.entries().last().unwrap().url, "https://new.example/");
    }

    #[test]
    fn a_reload_or_a_late_title_updates_the_newest_visit() {
        let mut visits = Visits::in_memory();
        visits.record("http://a.test/", "", 1).unwrap();
        visits.record("http://a.test/", "A", 2).unwrap();
        visits.record("http://a.test/", "", 3).unwrap();
        assert_eq!(visits.entries().len(), 1);
        assert_eq!(visits.entries()[0].title, "A");
    }

    #[test]
    fn only_web_pages_and_files_are_kept() {
        let mut visits = Visits::in_memory();
        for url in ["about:start", "data:text/html,x", "mailto:me@x"] {
            visits.record(url, "x", 1).unwrap();
        }
        visits.record("HTTPS://A.TEST/", "", 1).unwrap();
        visits.record("file:///docs/a.html", "", 1).unwrap();
        assert_eq!(visits.entries().len(), 2);
    }

    #[test]
    fn recent_lists_distinct_pages_newest_first() {
        let mut visits = Visits::in_memory();
        for (i, url) in ["http://a/", "http://b/", "http://a/", "http://c/"]
            .iter()
            .enumerate()
        {
            visits.record(url, "", i as u64).unwrap();
        }
        let urls: Vec<_> = visits.recent(2).iter().map(|v| v.url.as_str()).collect();
        assert_eq!(urls, ["http://c/", "http://a/"]);
    }

    #[test]
    fn the_file_stays_bounded_and_clear_empties_it() {
        let file = temp("bounded");
        let mut visits = Visits::open(file.clone());
        for i in 0..(2 * MAX_VISITS + 5) {
            visits
                .record(&format!("http://p{i}/"), "", i as u64)
                .unwrap();
        }
        let lines = fs::read_to_string(&file).unwrap().lines().count();
        assert!(lines < 2 * MAX_VISITS, "{lines} lines");
        let again = Visits::open(file.clone());
        assert_eq!(again.entries().len(), MAX_VISITS);
        assert_eq!(
            again.entries().last().unwrap().url,
            format!("http://p{}/", 2 * MAX_VISITS + 4)
        );
        visits.clear().unwrap();
        assert!(Visits::open(file).entries().is_empty());
    }

    #[test]
    fn broken_lines_are_skipped() {
        let file = temp("broken");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "x\thttp://a/\tA\n5\n7\thttp://b/\tB\n8\tjavascript:x\t\n",
        )
        .unwrap();
        let visits = Visits::open(file);
        assert_eq!(visits.entries().len(), 1);
        assert_eq!(visits.entries()[0].url, "http://b/");
    }
}

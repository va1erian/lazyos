//! The spool directory: one `<id>.job` file per job (its ticket and state,
//! one `key=value` line each) and one `<id>.doc` file (the document, while
//! the job needs it).
//!
//! The job file is what lets a restarted spooler finish what its last run
//! left: rewritten whole through a temp file and a rename on every state
//! change, so it is always one run's complete record. Values are display
//! text and printer choices; a line break in one is replaced, never stored,
//! so a value cannot forge another line.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{JobId, MAX_FIELD, State, Ticket};

/// Largest job file read back: a few short lines.
const MAX_JOB_FILE: u64 = 16 * 1024;

/// What the job file records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: JobId,
    pub owner: u32,
    pub state: State,
    /// `ipp://host:port/path`.
    pub printer: String,
    pub user: String,
    pub ticket: Ticket,
    /// The printer's id for the job once Create-Job answered.
    pub printer_job: Option<i32>,
    /// The last status line.
    pub line: String,
}

/// The spool directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store in `dir`, created (0700 on Unix) when missing.
    pub fn open(dir: &Path) -> io::Result<Store> {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Store {
            dir: dir.to_path_buf(),
        })
    }

    /// The document file of job `id`.
    pub fn document(&self, id: JobId) -> PathBuf {
        self.dir.join(format!("{id}.doc"))
    }

    fn record_path(&self, id: JobId) -> PathBuf {
        self.dir.join(format!("{id}.job"))
    }

    /// Writes `record`, replacing the last one.
    pub fn save(&self, record: &Record) -> io::Result<()> {
        let path = self.record_path(record.id);
        let temp = self.dir.join(format!("{}.job.new", record.id));
        let mut file = fs::File::create(&temp)?;
        file.write_all(encode(record).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)?;
        // The rename itself reaches the disk only with its directory.
        #[cfg(unix)]
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Forgets job `id`: both its files.
    pub fn remove(&self, id: JobId) {
        let _ = fs::remove_file(self.document(id));
        let _ = fs::remove_file(self.record_path(id));
    }

    /// Drops job `id`'s document, keeping its record.
    pub fn remove_document(&self, id: JobId) {
        let _ = fs::remove_file(self.document(id));
    }

    /// Every job the directory holds, by id. A record that does not read
    /// back is removed with its document; stray files are left alone.
    pub fn load(&self) -> io::Result<Vec<Record>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".job"))
                .and_then(|n| n.parse::<JobId>().ok())
            else {
                continue;
            };
            match read_record(&path).and_then(|text| decode(id, &text)) {
                Some(record) => records.push(record),
                None => self.remove(id),
            }
        }
        records.sort_by_key(|r| r.id);
        Ok(records)
    }
}

fn read_record(path: &Path) -> Option<String> {
    if fs::metadata(path).ok()?.len() > MAX_JOB_FILE {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// A value as stored: one line, bounded.
fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_FIELD)
        .collect()
}

fn encode(r: &Record) -> String {
    let mut out = String::new();
    let mut line = |key: &str, value: &str| {
        out.push_str(key);
        out.push('=');
        out.push_str(&clean(value));
        out.push('\n');
    };
    line("owner", &r.owner.to_string());
    line("state", r.state.word());
    line("printer", &r.printer);
    line("user", &r.user);
    line("name", &r.ticket.name);
    line("format", &r.ticket.format);
    if let Some(copies) = r.ticket.copies {
        line("copies", &copies.to_string());
    }
    if let Some(media) = &r.ticket.media {
        line("media", media);
    }
    if let Some(mode) = &r.ticket.color_mode {
        line("color", mode);
    }
    if let Some(quality) = r.ticket.quality {
        line("quality", &quality.to_string());
    }
    if let Some(job) = r.printer_job {
        line("printer-job", &job.to_string());
    }
    line("line", &r.line);
    out
}

fn decode(id: JobId, text: &str) -> Option<Record> {
    let mut record = Record {
        id,
        owner: u32::MAX,
        state: State::Failed,
        printer: String::new(),
        user: String::new(),
        ticket: Ticket::default(),
        printer_job: None,
        line: String::new(),
    };
    let (mut owner, mut state) = (None, None);
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        let value = value.to_owned();
        match key {
            "owner" => owner = Some(value.parse().ok()?),
            "state" => state = Some(State::from_word(&value)?),
            "printer" => record.printer = value,
            "user" => record.user = value,
            "name" => record.ticket.name = value,
            "format" => record.ticket.format = value,
            "copies" => record.ticket.copies = Some(value.parse().ok()?),
            "media" => record.ticket.media = Some(value),
            "color" => record.ticket.color_mode = Some(value),
            "quality" => record.ticket.quality = Some(value.parse().ok()?),
            "printer-job" => record.printer_job = Some(value.parse().ok()?),
            "line" => record.line = value,
            // A key a later version wrote: keep the rest.
            _ => {}
        }
    }
    record.owner = owner?;
    record.state = state?;
    if record.printer.is_empty() {
        return None;
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            id: 7,
            owner: 1000,
            state: State::Sending,
            printer: "ipp://192.168.1.89:631/ipp/print".into(),
            user: "alice".into(),
            ticket: Ticket {
                name: "Letter\nstate=done".into(),
                format: "image/pwg-raster".into(),
                copies: Some(2),
                media: Some("iso_a4_210x297mm".into()),
                color_mode: Some("monochrome".into()),
                quality: Some(5),
            },
            printer_job: Some(42),
            line: "Sending the pages...".into(),
        }
    }

    #[test]
    fn a_record_round_trips_and_a_line_break_cannot_forge_a_key() {
        let dir = std::env::temp_dir().join(format!("printd-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        let mut r = record();
        store.save(&r).unwrap();
        let back = store.load().unwrap();
        r.ticket.name = "Letter state=done".into();
        assert_eq!(back, vec![r]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_record_is_dropped_with_its_document() {
        let dir = std::env::temp_dir().join(format!("printd-broken-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        fs::write(dir.join("3.job"), "owner=x\nstate=queued\n").unwrap();
        fs::write(dir.join("3.doc"), b"RaS2").unwrap();
        fs::write(dir.join("notes.txt"), b"left alone").unwrap();
        assert!(store.load().unwrap().is_empty());
        assert!(!dir.join("3.doc").exists());
        assert!(dir.join("notes.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}

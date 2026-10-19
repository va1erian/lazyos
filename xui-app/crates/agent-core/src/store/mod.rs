//! The conversation store (docs/ai-agent-plan.md §9.3): one append-only
//! JSONL file per conversation under `<app dir>/conversations`, deleted ones
//! moved to `<app dir>/trash`. A file is never rewritten, so a crash loses at
//! most the line being written, and a damaged line is skipped, not fatal.

mod conversation;
mod event;

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

pub use conversation::{Conversation, Item, SUMMARY_FRAME};
pub use event::{Event, Persona};

/// A file bigger than this is not read: it is not a conversation.
const MAX_FILE: u64 = 256 * 1024 * 1024;
/// A line bigger than this is skipped.
const MAX_LINE: usize = 8 * 1024 * 1024;

pub struct Store {
    conversations: PathBuf,
    trash: PathBuf,
    /// Conversations whose end this process has checked for a torn line: a
    /// crash tears a file between runs, so once per run is enough.
    tail_checked: Mutex<HashSet<String>>,
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub created: u64,
    /// Seconds since the epoch of the file's last write.
    pub updated: u64,
    pub model: String,
}

/// A search hit: where the text was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: String,
    pub title: String,
    /// A short stretch of text around the match, or the title.
    pub snippet: String,
}

impl Store {
    pub fn new(app_dir: &Path) -> Store {
        Store {
            conversations: app_dir.join("conversations"),
            trash: app_dir.join("trash"),
            tail_checked: Mutex::default(),
        }
    }

    /// Starts a conversation: a fresh id and its `Meta` line.
    pub fn create(
        &self,
        created: u64,
        model: &str,
        persona: Persona,
        diagnostics: bool,
        system: Option<String>,
    ) -> io::Result<Conversation> {
        private_dir(&self.conversations)?;
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        loop {
            let id = format!(
                "c{created:x}{:03x}",
                COUNTER.fetch_add(1, Ordering::Relaxed) & 0xfff
            );
            let meta = Event::Meta {
                id: id.clone(),
                created,
                model: model.to_string(),
                persona,
                diagnostics,
                system: system.clone(),
            };
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            match options.open(self.path(&id)?) {
                Ok(mut file) => {
                    file.write_all(format!("{}\n", meta.to_line()).as_bytes())?;
                    return Ok(Conversation::replay([meta]).expect("a meta replays"));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Appends one event as one line (a single write). A file that does not
    /// end in a new line (a write cut short by a crash) gets one first, so
    /// the new event is never glued onto the torn one. An event whose line
    /// would be over [`MAX_LINE`] is refused: the reader would skip it.
    pub fn append(&self, id: &str, event: &Event) -> io::Result<()> {
        let line = event.to_line();
        if line.len() >= MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the event is too large to store",
            ));
        }
        let path = self.path(id)?;
        let first = self
            .tail_checked
            .lock()
            .map_or(true, |mut seen| seen.insert(id.to_string()));
        let mut file = OpenOptions::new().read(first).append(true).open(path)?;
        let mut out = String::with_capacity(line.len() + 2);
        let len = if first { file.metadata()?.len() } else { 0 };
        if len > 0 {
            file.seek(SeekFrom::Start(len - 1))?;
            let mut last = [0u8; 1];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                out.push('\n');
            }
        }
        out.push_str(&line);
        out.push('\n');
        file.write_all(out.as_bytes())
    }

    pub fn load(&self, id: &str) -> io::Result<Conversation> {
        let events = read_events(&self.path(id)?)?;
        let mut conversation = Conversation::replay(events)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a conversation file"))?;
        // The file name is the identity: a copied file must not append to
        // the one it was copied from.
        conversation.id = id.to_string();
        Ok(conversation)
    }

    /// Every conversation, the most recently written first.
    pub fn list(&self) -> Vec<Summary> {
        let Ok(dir) = fs::read_dir(&self.conversations) else {
            return Vec::new();
        };
        let mut rows: Vec<Summary> = dir
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_str()?.to_string();
                if path.extension()? != "jsonl" || !valid_id(&id) {
                    return None;
                }
                let updated = entry
                    .metadata()
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let conversation = self.load(&id).ok()?;
                Some(Summary {
                    title: conversation
                        .title
                        .clone()
                        .unwrap_or_else(|| first_words(&conversation)),
                    id,
                    created: conversation.created,
                    updated,
                    model: conversation.model,
                })
            })
            .collect();
        rows.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        rows
    }

    /// Moves a conversation to the trash folder; nothing is hard-deleted.
    pub fn delete(&self, id: &str) -> io::Result<()> {
        private_dir(&self.trash)?;
        let from = self.path(id)?;
        let mut to = self.trash.join(format!("{id}.jsonl"));
        let mut n = 1;
        while to.exists() {
            to = self.trash.join(format!("{id}-{n}.jsonl"));
            n += 1;
        }
        fs::rename(from, to)
    }

    /// Case-insensitive search over titles and the text of every message.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for row in self.list() {
            if row.title.to_lowercase().contains(&needle) {
                hits.push(Hit {
                    id: row.id.clone(),
                    title: row.title.clone(),
                    snippet: row.title.clone(),
                });
                continue;
            }
            let Ok(conversation) = self.load(&row.id) else {
                continue;
            };
            let found = conversation.items.iter().find_map(|item| match item {
                Item::User { text } | Item::Summary { text } => around(text, &needle),
                Item::Assistant { text, .. } => around(text, &needle),
                Item::ToolResult { .. } => None,
            });
            if let Some(snippet) = found {
                hits.push(Hit {
                    id: row.id,
                    title: row.title,
                    snippet,
                });
            }
        }
        hits
    }

    /// Removes the conversations and the trash (the Delete all data button).
    pub fn delete_everything(&self) -> io::Result<()> {
        for dir in [&self.conversations, &self.trash] {
            match fs::remove_dir_all(dir) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// The id becomes a file name, so it is checked first.
    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a conversation id",
            ));
        }
        Ok(self.conversations.join(format!("{id}.jsonl")))
    }
}

/// Makes `dir` (and its parents) readable by its owner alone: the
/// conversations hold system logs the user may not want shared.
fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn read_events(path: &Path) -> io::Result<Vec<Event>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file is too large",
        ));
    }
    let mut events = Vec::new();
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        // The cap bounds one line even in a file with no new line at all.
        let read = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_LINE {
            // Skip the rest of the oversized line.
            loop {
                let mut sink = Vec::new();
                let n = reader.by_ref().take(1 << 20).read_until(b'\n', &mut sink)?;
                if n == 0 || sink.ends_with(b"\n") {
                    break;
                }
            }
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        if let Ok(event) = Event::from_line(text.trim_end()) {
            events.push(event);
        }
    }
    Ok(events)
}

fn first_words(conversation: &Conversation) -> String {
    let text = conversation.items.iter().find_map(|i| match i {
        Item::User { text } => Some(text.as_str()),
        _ => None,
    });
    match text {
        Some(t) => {
            let line: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut end = line.len().min(60);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].to_string()
        }
        None => "New chat".to_string(),
    }
}

/// About 80 bytes of `text` around the first match of `needle` (lowercase).
fn around(text: &str, needle: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // Lowercasing can change byte lengths; positions are mapped back by chars.
    let at = lower.find(needle)?;
    let char_index = lower[..at].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let from = char_index.saturating_sub(30);
    let to = (char_index + needle.chars().count() + 50).min(chars.len());
    let snippet: String = chars[from.min(chars.len())..to].iter().collect();
    Some(snippet.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests;

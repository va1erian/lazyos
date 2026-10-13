//! The sites a person has allowed or blocked to notify, kept in the profile:
//! `$HOME/.apps/os.lazy.lazywebplus/notify.tsv`, one decision per line,
//! `<origin>\t<allow|block>\t<unix seconds>`.
//!
//! The store is shared (a clone is the same store): the UI process reads it on WebKit's thread when
//! the engine asks which sites may notify, and the window changes it on its own. Only `http:` and
//! `https:` origins in WebKit's own spelling are ever kept, so a hand-edited or hostile file adds
//! nothing the window would not have written, and the file never grows past [`MAX_SITES`] lines.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::origin::Origin;

/// How many decisions are kept; the oldest goes when a new one needs the room.
pub const MAX_SITES: usize = 512;
/// How much of the file is read.
const MAX_READ: u64 = 256 * 1024;

/// What the person said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}

impl Decision {
    fn word(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Block => "block",
        }
    }

    fn from_word(word: &str) -> Option<Decision> {
        match word {
            "allow" => Some(Decision::Allow),
            "block" => Some(Decision::Block),
            _ => None,
        }
    }
}

/// One site's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    /// The origin as [`Origin::key`] writes it.
    pub origin: String,
    pub decision: Decision,
    /// When it was made, in seconds since the epoch.
    pub time: u64,
}

#[derive(Default)]
struct Inner {
    sites: Vec<Site>,
    file: Option<PathBuf>,
}

/// The decisions, shared by every clone.
#[derive(Clone, Default)]
pub struct Permissions {
    inner: Arc<Mutex<Inner>>,
    /// Taken for the length of a file write, which is done with `inner` released: the UI
    /// process reads the decisions on WebKit's run loop and must never wait for a disk.
    writing: Arc<Mutex<()>>,
}

impl Permissions {
    /// Decisions kept only in memory (no home to keep them in).
    pub fn in_memory() -> Permissions {
        Permissions::default()
    }

    /// The decisions saved in `file`. A missing or unreadable file is no decisions; a line that
    /// is not a decision about a web origin is skipped.
    pub fn open(file: PathBuf) -> Permissions {
        let text = read(&file);
        let mut sites: Vec<Site> = Vec::new();
        for site in text.lines().filter_map(parse) {
            // A later line is a later decision about the same site.
            sites.retain(|old| old.origin != site.origin);
            sites.push(site);
        }
        let excess = sites.len().saturating_sub(MAX_SITES);
        sites.drain(..excess);
        Permissions {
            inner: Arc::new(Mutex::new(Inner {
                sites,
                file: Some(file),
            })),
            writing: Arc::default(),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the person decided about `origin` (an [`Origin::key`]), if anything.
    pub fn decision(&self, origin: &str) -> Option<Decision> {
        self.inner()
            .sites
            .iter()
            .find(|site| site.origin == origin)
            .map(|site| site.decision)
    }

    /// Records `decision` for `origin` and saves the file. Returns the error of a save that
    /// failed: the decision holds for this session regardless.
    pub fn set(&self, origin: &Origin, decision: Decision, time: u64) -> io::Result<()> {
        let (file, sites) = {
            let mut inner = self.inner();
            let key = origin.key();
            inner.sites.retain(|site| site.origin != key);
            if inner.sites.len() >= MAX_SITES {
                inner.sites.remove(0);
            }
            inner.sites.push(Site {
                origin: key,
                decision,
                time,
            });
            (inner.file.clone(), inner.sites.clone())
        };
        self.save(file, &sites)
    }

    /// Forgets the decision about `origin`; whether there was one.
    pub fn forget(&self, origin: &str) -> io::Result<bool> {
        let (file, sites) = {
            let mut inner = self.inner();
            let before = inner.sites.len();
            inner.sites.retain(|site| site.origin != origin);
            if inner.sites.len() == before {
                return Ok(false);
            }
            (inner.file.clone(), inner.sites.clone())
        };
        self.save(file, &sites).map(|()| true)
    }

    /// Writes `sites` to `file`, with the decisions' lock free.
    fn save(&self, file: Option<PathBuf>, sites: &[Site]) -> io::Result<()> {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        write(file, sites)
    }

    /// Every decision, in the order they were made.
    pub fn sites(&self) -> Vec<Site> {
        self.inner().sites.clone()
    }

    /// Decision `index` (in the order of [`Permissions::sites`]) as the origin and whether it
    /// allows.
    pub fn at(&self, index: usize) -> Option<(String, bool)> {
        self.inner()
            .sites
            .get(index)
            .map(|site| (site.origin.clone(), site.decision == Decision::Allow))
    }
}

fn read(file: &PathBuf) -> String {
    use std::io::Read;
    let Ok(f) = fs::File::open(file) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if f.take(MAX_READ).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Writes the whole file through a temporary one, so a crash leaves the old decisions.
fn write(file: Option<PathBuf>, sites: &[Site]) -> io::Result<()> {
    let Some(file) = file else {
        return Ok(());
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let text: String = sites
        .iter()
        .map(|site| format!("{}\t{}\t{}\n", site.origin, site.decision.word(), site.time))
        .collect();
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &file)
}

fn parse(line: &str) -> Option<Site> {
    let mut fields = line.split('\t');
    let origin = fields.next()?;
    let decision = Decision::from_word(fields.next()?)?;
    let time = fields.next()?.trim().parse().ok()?;
    // Only an origin written the way WebKit writes it: what the engine will ask about.
    let canonical = Origin::from_url(origin)?;
    (canonical.key() == origin).then(|| Site {
        origin: origin.to_owned(),
        decision,
        time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazywebplus-notify-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sub").join("notify.tsv")
    }

    fn origin(url: &str) -> Origin {
        Origin::from_url(url).unwrap()
    }

    #[test]
    fn decisions_survive_a_restart_and_the_file_is_the_documented_one() {
        let file = temp("restart");
        let store = Permissions::open(file.clone());
        store
            .set(&origin("https://a.test/page"), Decision::Allow, 10)
            .unwrap();
        store
            .set(&origin("http://b.test:8080/"), Decision::Block, 20)
            .unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "https://a.test\tallow\t10\nhttp://b.test:8080\tblock\t20\n"
        );
        let again = Permissions::open(file);
        assert_eq!(again.decision("https://a.test"), Some(Decision::Allow));
        assert_eq!(again.decision("http://b.test:8080"), Some(Decision::Block));
        assert_eq!(again.decision("https://c.test"), None);
        assert_eq!(again.at(0), Some(("https://a.test".into(), true)));
        assert_eq!(again.at(1), Some(("http://b.test:8080".into(), false)));
        assert_eq!(again.at(2), None);
    }

    #[test]
    fn a_new_decision_replaces_the_old_one_and_forgetting_removes_it() {
        let file = temp("replace");
        let store = Permissions::open(file.clone());
        let a = origin("https://a.test");
        store.set(&a, Decision::Allow, 1).unwrap();
        store.set(&a, Decision::Block, 2).unwrap();
        assert_eq!(store.sites().len(), 1);
        assert_eq!(store.decision("https://a.test"), Some(Decision::Block));
        assert!(store.forget("https://a.test").unwrap());
        assert!(!store.forget("https://a.test").unwrap());
        assert_eq!(Permissions::open(file).sites(), Vec::new());
    }

    #[test]
    fn a_hostile_file_adds_nothing_the_window_would_not_have_written() {
        let file = temp("hostile");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "data:text/html,x\tallow\t1\n\
             about:blank\tallow\t1\n\
             https://A.test\tallow\t1\n\
             https://a.test/path\tallow\t1\n\
             https://ok.test\tmaybe\t1\n\
             https://ok.test\tallow\tsoon\n\
             \u{0}\u{0}\n\
             https://good.test\tallow\t5\n\
             https://good.test\tblock\t6\n",
        )
        .unwrap();
        let store = Permissions::open(file);
        assert_eq!(
            store.sites(),
            vec![Site {
                origin: "https://good.test".into(),
                decision: Decision::Block,
                time: 6
            }]
        );
    }

    #[test]
    fn the_oldest_decision_makes_room() {
        let store = Permissions::in_memory();
        for n in 0..(MAX_SITES + 5) {
            store
                .set(
                    &origin(&format!("https://s{n}.test")),
                    Decision::Allow,
                    n as u64,
                )
                .unwrap();
        }
        let sites = store.sites();
        assert_eq!(sites.len(), MAX_SITES);
        assert_eq!(sites[0].origin, "https://s5.test");
        assert_eq!(store.decision("https://s0.test"), None);
    }

    #[test]
    fn clones_are_one_store() {
        let store = Permissions::in_memory();
        let other = store.clone();
        store
            .set(&origin("https://a.test"), Decision::Allow, 1)
            .unwrap();
        assert_eq!(other.decision("https://a.test"), Some(Decision::Allow));
    }
}

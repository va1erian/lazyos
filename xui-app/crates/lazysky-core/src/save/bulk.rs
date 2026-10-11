//! "Download all images" of an account (§7.6 step 4): a scan of its
//! media posts through the cursor, 100 ms between pages, handing each
//! picture to the save threads as it is found; progress and a final count.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::pool::{Outcome, SaveJob, SavePool};
use super::sanitize::{extension_of, sanitize};
use crate::net::api::{Api, ApiError, Notes};
use crate::proto::Page;
use crate::{Event, Platform};

/// Posts per page of the scan.
const SCAN_PAGE: &str = "100";

/// The counters a bulk run shares between the scan and the save threads.
pub struct BulkState {
    pub seq: u64,
    platform: Arc<dyn Platform>,
    pub cancel: Arc<AtomicBool>,
    queued: AtomicUsize,
    saved: AtomicUsize,
    skipped: AtomicUsize,
    errors: AtomicUsize,
    cancelled: AtomicUsize,
    scanned: AtomicUsize,
    scan_done: AtomicBool,
    announced: AtomicBool,
}

impl BulkState {
    pub fn new(seq: u64, cancel: Arc<AtomicBool>, platform: Arc<dyn Platform>) -> BulkState {
        BulkState {
            seq,
            platform,
            cancel,
            queued: AtomicUsize::new(0),
            saved: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            errors: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            scanned: AtomicUsize::new(0),
            scan_done: AtomicBool::new(false),
            announced: AtomicBool::new(false),
        }
    }

    fn finished_total(&self) -> usize {
        self.saved.load(Ordering::SeqCst)
            + self.skipped.load(Ordering::SeqCst)
            + self.errors.load(Ordering::SeqCst)
            + self.cancelled.load(Ordering::SeqCst)
    }

    fn progress(&self) -> Event {
        Event::BulkProgress {
            seq: self.seq,
            scanned_posts: self.scanned.load(Ordering::SeqCst),
            found: self.queued.load(Ordering::SeqCst),
            finished: self.finished_total(),
            scanning: !self.scan_done.load(Ordering::SeqCst),
        }
    }

    /// The final event, once, when the scan is over and every job answered.
    fn maybe_done(&self) -> Option<Event> {
        let over = self.scan_done.load(Ordering::SeqCst)
            && self.finished_total() >= self.queued.load(Ordering::SeqCst);
        if over && !self.announced.swap(true, Ordering::SeqCst) {
            let saved = self.saved.load(Ordering::SeqCst);
            let skipped = self.skipped.load(Ordering::SeqCst);
            let errors = self.errors.load(Ordering::SeqCst);
            self.platform
                .log(&format!("SKY:BULK:DONE:{}:{errors}", saved + skipped));
            Some(Event::BulkDone {
                seq: self.seq,
                saved,
                skipped,
                errors,
                cancelled: self.cancel.load(Ordering::SeqCst),
            })
        } else {
            None
        }
    }

    /// A save thread finished one job: the events to send.
    pub(super) fn finished(&self, outcome: Outcome) -> Vec<Event> {
        let counter = match outcome {
            Outcome::Saved => &self.saved,
            Outcome::Skipped => &self.skipped,
            Outcome::Failed => &self.errors,
            Outcome::Cancelled => &self.cancelled,
        };
        counter.fetch_add(1, Ordering::SeqCst);
        match self.maybe_done() {
            Some(done) => vec![self.progress(), done],
            None => vec![self.progress()],
        }
    }
}

/// What a scan step did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// More pages remain: call again (after a short wait).
    More,
    /// The scan is over (the saves may still be running).
    Done,
}

/// One account's media scan.
pub struct Scan {
    state: Arc<BulkState>,
    actor: String,
    dir: PathBuf,
    cursor: Option<String>,
    made_dir: bool,
}

impl Scan {
    pub fn new(state: Arc<BulkState>, actor: String, dir: PathBuf) -> Scan {
        Scan {
            state,
            actor,
            dir,
            cursor: None,
            made_dir: false,
        }
    }

    pub fn state(&self) -> &Arc<BulkState> {
        &self.state
    }

    /// Reads one page of the account's media posts and queues its pictures.
    pub fn step(
        &mut self,
        api: &mut Api,
        pool: &SavePool,
        notes: &dyn Notes,
        events: &std::sync::mpsc::Sender<Event>,
    ) -> Result<Step, ApiError> {
        if self.state.cancel.load(Ordering::SeqCst) {
            return Ok(self.finish(events));
        }
        let mut query: Vec<(&str, &str)> = vec![
            ("actor", &self.actor),
            ("filter", "posts_with_media"),
            ("limit", SCAN_PAGE),
        ];
        if let Some(cursor) = self.cursor.as_deref() {
            query.push(("cursor", cursor));
        }
        let value = api.get("app.bsky.feed.getAuthorFeed", &query, notes)?;
        let page = Page::from_feed(&value);
        self.state
            .scanned
            .fetch_add(page.items.len() + page.skipped, Ordering::SeqCst);
        for item in &page.items {
            // Only the account's own non-reply posts: no reposts of other
            // people's pictures.
            let post = &item.post;
            let own = post.author.did == self.actor || post.author.handle == self.actor;
            if item.reason.is_some() || !own || post.reply.is_some() {
                continue;
            }
            for image in post.embed.images() {
                if !self.made_dir {
                    let _ = std::fs::create_dir_all(&self.dir);
                    self.made_dir = true;
                }
                let name = image_file_name(&post.created_at, &image.fullsize);
                self.state.queued.fetch_add(1, Ordering::SeqCst);
                pool.submit(SaveJob {
                    seq: self.state.seq,
                    url: image.fullsize.clone(),
                    path: self.dir.join(name),
                    bulk: Some(Arc::clone(&self.state)),
                });
            }
        }
        let _ = events.send(self.state.progress());
        self.cursor = page.cursor;
        if self.cursor.is_none() {
            return Ok(self.finish(events));
        }
        Ok(Step::More)
    }

    /// Ends the scan: the final event goes out now if every job already
    /// answered (or there were none).
    pub fn finish(&self, events: &std::sync::mpsc::Sender<Event>) -> Step {
        self.state.scan_done.store(true, Ordering::SeqCst);
        if let Some(done) = self.state.maybe_done() {
            let _ = events.send(done);
        }
        Step::Done
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// `img<date>_<cid prefix>.<ext>` for a picture of a post written at
/// `created_at`.
pub fn image_file_name(created_at: &str, url: &str) -> String {
    let date = created_at.replace(':', "-");
    let cid = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let cid: String = cid.chars().take(12).collect();
    sanitize(&format!("img{date}_{cid}.{}", extension_of(url)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Roots;

    struct Quiet;

    impl Platform for Quiet {
        fn ca_roots(&self) -> Roots {
            Roots::Pem(Arc::default())
        }
        fn app_dir(&self) -> PathBuf {
            PathBuf::new()
        }
        fn pictures_dir(&self) -> PathBuf {
            PathBuf::new()
        }
        fn open_url(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn log(&self, _: &str) {}
    }

    #[test]
    fn file_names_carry_the_date_and_the_cid() {
        let url =
            "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:x/bafkreiabcdefghijklmnop@jpeg";
        assert_eq!(
            image_file_name("2026-10-10T12:30:00.000Z", url),
            "img2026-10-10T12-30-00.000Z_bafkreiabcde.jpg"
        );
        // A hostile URL still gives a plain name.
        let name = image_file_name("../../x", "https://h/../../etc/passwd@png");
        assert!(!name.contains('/') && !name.starts_with('.'), "{name}");
    }

    #[test]
    fn a_run_with_nothing_to_do_announces_itself_once() {
        let state = BulkState::new(1, Arc::default(), Arc::new(Quiet));
        assert!(state.maybe_done().is_none(), "the scan is not over");
        state.scan_done.store(true, Ordering::SeqCst);
        assert!(matches!(
            state.maybe_done(),
            Some(Event::BulkDone { saved: 0, .. })
        ));
        assert!(state.maybe_done().is_none(), "announced once");
    }

    #[test]
    fn the_last_job_ends_the_run() {
        let state = BulkState::new(2, Arc::default(), Arc::new(Quiet));
        state.queued.store(2, Ordering::SeqCst);
        state.scan_done.store(true, Ordering::SeqCst);
        let first = state.finished(Outcome::Saved);
        assert_eq!(first.len(), 1);
        let second = state.finished(Outcome::Failed);
        assert!(matches!(
            &second[1],
            Event::BulkDone {
                saved: 1,
                errors: 1,
                ..
            }
        ));
    }
}

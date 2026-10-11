//! The image threads (§7.1, R6): a few workers, each with its own HTTP
//! client, taking the wanted list from the front.
//!
//! The list is *replaced* by each [`ImagePool::set_wanted`] (the UI sends
//! the pictures near the view, nearest first): what still waits and is no
//! longer wanted is dropped before it starts. A picture already being
//! fetched finishes; it is the UI's cache that decides whether it is kept.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{circle_mask, cover, decode_limited, fit, Bitmap, ImageKey, Shape};
use crate::net::{Http, HttpOptions};
use crate::platform::Roots;
use crate::Event;

/// Image threads. Four overlap network waits without crowding a small
/// machine; decoding is the part that is CPU-bound.
pub const THREADS: usize = 4;
/// The most a picture's bytes may be.
const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct Queue {
    wanted: VecDeque<ImageKey>,
    /// Keys being fetched now (not re-queued by a later list).
    running: HashSet<ImageKey>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

pub(crate) struct ImagePool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl ImagePool {
    pub(crate) fn start(roots: Roots, events: Sender<Event>, threads: usize) -> ImagePool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let threads = (0..threads.max(1))
            .map(|n| {
                let (shared, roots, events) = (Arc::clone(&shared), roots.clone(), events.clone());
                thread::Builder::new()
                    .name(format!("sky-img{n}"))
                    .spawn(move || run(&shared, roots, &events))
                    .expect("an image thread starts")
            })
            .collect();
        ImagePool { shared, threads }
    }

    /// Replaces the waiting list. Keys being fetched right now are not
    /// queued again.
    pub(crate) fn set_wanted(&self, keys: Vec<ImageKey>) {
        let mut queue = self.lock();
        let mut seen = HashSet::new();
        queue.wanted = keys
            .into_iter()
            .filter(|key| !queue.running.contains(key) && seen.insert(key.clone()))
            .collect();
        drop(queue);
        self.shared.wake.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for ImagePool {
    fn drop(&mut self) {
        self.lock().closed = true;
        self.shared.wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, roots: Roots, events: &Sender<Event>) {
    // Made on this thread: its sockets are this thread's own.
    let options = HttpOptions {
        max_body: MAX_BODY,
        read_timeout: Duration::from_secs(30),
        ..HttpOptions::default()
    };
    let http = Http::new(roots, options);
    loop {
        let key = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(key) = queue.wanted.pop_front() {
                    queue.running.insert(key.clone());
                    break key;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = load(&http, &key);
        shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
            .remove(&key);
        if events.send(Event::Image { key, result }).is_err() {
            return;
        }
    }
}

/// Fetches, decodes and fits one picture.
fn load(http: &Http, key: &ImageKey) -> Result<Bitmap, String> {
    // Video thumbnails answer with a redirect (video.bsky.app); a picture
    // is public, so following up to three hops to https is safe.
    let response = http.get_following(&key.url, &[("accept", "image/*")], 3)?;
    if response.status != 200 {
        return Err(format!("the picture server answered {}", response.status));
    }
    let img = decode_limited(&response.body)?;
    let bitmap = match key.shape {
        Shape::Cover => cover(&img, key.width, key.height),
        Shape::Circle => {
            let mut bitmap = cover(&img, key.width, key.height);
            circle_mask(&mut bitmap);
            bitmap
        }
        Shape::Fit => fit(&img, key.width, key.height),
    };
    Ok(bitmap)
}

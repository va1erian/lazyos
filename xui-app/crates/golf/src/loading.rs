//! The course generator on a worker thread, so the window keeps painting and
//! answering input while a slow CPU grinds through the search.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::course::Course;
use crate::gen::{Generator, Params};

/// A course being generated on a worker thread, so the window keeps
/// painting and answering input while a slow CPU grinds through the search.
pub struct Loading {
    pub rx: Receiver<Course>,
    progress: Arc<Mutex<(String, f32)>>,
    cancel: Arc<AtomicBool>,
    pub started: Instant,
    /// The progress the last tick showed, to repaint only on a change.
    pub shown: (String, f32),
    pub failed: bool,
}

impl Loading {
    pub fn start(params: Params) -> Loading {
        let (tx, rx) = channel();
        let progress = Arc::new(Mutex::new(Generator::new(params).progress()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (shared, stop) = (Arc::clone(&progress), Arc::clone(&cancel));
        let spawned = std::thread::Builder::new()
            .name("golf-gen".into())
            .spawn(move || {
                let mut generator = Generator::new(params);
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(mut p) = shared.lock() {
                        *p = generator.progress();
                    }
                    if let Some(course) = generator.step() {
                        let _ = tx.send(course);
                        return;
                    }
                }
            });
        Loading {
            rx,
            progress,
            cancel,
            started: Instant::now(),
            shown: (String::new(), -1.0),
            failed: spawned.is_err(),
        }
    }

    pub fn snapshot(&self) -> (String, f32) {
        if self.failed {
            return ("Generation failed".into(), 0.0);
        }
        self.progress
            .lock()
            .map_or_else(|_| ("Generation failed".into(), 0.0), |p| p.clone())
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        // A superseded search stops at its next step.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

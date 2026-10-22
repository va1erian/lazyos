//! An [`Engine`] for another thread: the ad blocker's worker compiles and loads its rule lists from a
//! thread of its own, but the engine a window holds (a client of webd) lives on the window's thread,
//! whose Messenger handles are its own. A [`Relay`] turns each call into a job the window's thread
//! runs on the real engine ([`Jobs::run`]), and wakes that thread when the first job of a burst is
//! queued.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use crate::engine::{Button, Engine, MouseKind, NotifyAnswer};

type Job = Box<dyn FnOnce(&dyn Engine) + Send>;
type Wake = Arc<dyn Fn() + Send + Sync>;

/// The calling side: `Send + Sync`, every call queued.
pub struct Relay {
    jobs: Mutex<Sender<Job>>,
    wake: Wake,
}

/// The window's side: the jobs to run on the real engine.
pub struct Jobs {
    jobs: Receiver<Job>,
}

/// A relay and its jobs; `wake` is called (from the caller's thread) for every job queued.
pub fn relay(wake: impl Fn() + Send + Sync + 'static) -> (Relay, Jobs) {
    let (sender, jobs) = channel();
    (
        Relay {
            jobs: Mutex::new(sender),
            wake: Arc::new(wake),
        },
        Jobs { jobs },
    )
}

impl Jobs {
    /// Runs every queued call on `engine`, oldest first; how many ran.
    pub fn run(&self, engine: &dyn Engine) -> usize {
        let mut ran = 0;
        while let Ok(job) = self.jobs.try_recv() {
            job(engine);
            ran += 1;
        }
        ran
    }
}

impl Relay {
    fn queue(&self, job: impl FnOnce(&dyn Engine) + Send + 'static) {
        let sent = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(Box::new(job));
        if sent.is_ok() {
            (self.wake)();
        }
    }
}

impl Engine for Relay {
    fn load(&self, url: &str) {
        let url = url.to_owned();
        self.queue(move |e| e.load(&url));
    }
    fn reload(&self) {
        self.queue(|e| e.reload());
    }
    fn stop(&self) {
        self.queue(|e| e.stop());
    }
    fn recover(&self, reload: bool) {
        self.queue(move |e| e.recover(reload));
    }
    fn back(&self) {
        self.queue(|e| e.back());
    }
    fn forward(&self) {
        self.queue(|e| e.forward());
    }
    fn resize(&self, width: i32, height: i32) {
        self.queue(move |e| e.resize(width, height));
    }
    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32) {
        self.queue(move |e| e.mouse(kind, button, x, y, mods));
    }
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        self.queue(move |e| e.wheel(x, y, ticks_x, ticks_y, mods));
    }
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        let text = text.to_owned();
        self.queue(move |e| e.key(press, keysym, &text, mods));
    }
    fn focus(&self, focused: bool) {
        self.queue(move |e| e.focus(focused));
    }
    fn download(&self, url: &str) {
        let url = url.to_owned();
        self.queue(move |e| e.download(&url));
    }
    fn download_cancel(&self, id: u64) {
        self.queue(move |e| e.download_cancel(id));
    }
    fn dialog_reply(&self, id: u64, accepted: bool, value: &str) {
        let value = value.to_owned();
        self.queue(move |e| e.dialog_reply(id, accepted, &value));
    }
    fn notify_shown(&self, id: u64) {
        self.queue(move |e| e.notify_shown(id));
    }
    fn notify_clicked(&self, id: u64) {
        self.queue(move |e| e.notify_clicked(id));
    }
    fn notify_closed(&self, id: u64) {
        self.queue(move |e| e.notify_closed(id));
    }
    fn notify_permission(&self, origin: &str, answer: NotifyAnswer) {
        let origin = origin.to_owned();
        self.queue(move |e| e.notify_permission(&origin, answer));
    }
    fn notify_forget(&self, origin: &str) {
        let origin = origin.to_owned();
        self.queue(move |e| e.notify_forget(&origin));
    }
    fn blocking_load(&self, id: &str) {
        let id = id.to_owned();
        self.queue(move |e| e.blocking_load(&id));
    }
    fn blocking_compile(&self, id: &str, json: &str) {
        let (id, json) = (id.to_owned(), json.to_owned());
        self.queue(move |e| e.blocking_compile(&id, &json));
    }
    fn blocking_forget(&self, id: &str) {
        let id = id.to_owned();
        self.queue(move |e| e.blocking_forget(&id));
    }
    fn set_blocking(&self, enabled: bool, exceptions: &[String]) {
        let exceptions = exceptions.to_vec();
        self.queue(move |e| e.set_blocking(enabled, &exceptions));
    }
    fn reload_unblocked(&self) {
        self.queue(|e| e.reload_unblocked());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine::{Call, Recorder};

    #[test]
    fn calls_from_another_thread_run_in_order_on_the_window_thread() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let (relay, jobs) = relay(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let worker = std::thread::spawn(move || {
            relay.blocking_load("easylist-0");
            relay.blocking_compile("easylist-1", "[]");
            relay.blocking_forget("old");
        });
        worker.join().unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 3);
        let engine = Recorder::default();
        assert_eq!(jobs.run(&engine), 3);
        assert_eq!(
            engine.calls(),
            vec![
                Call::BlockingLoad("easylist-0".into()),
                Call::BlockingCompile("easylist-1".into(), 2),
                Call::BlockingForget("old".into()),
            ]
        );
        assert_eq!(jobs.run(&engine), 0, "each job runs once");
    }
}

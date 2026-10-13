//! The window's line to the shell, on a thread of its own.
//!
//! A toast is best effort and the shell answers from its heartbeat, so the window never waits for
//! one: it hands the request to this worker and collects what came back (the toast's id, a click,
//! a close) when the worker wakes it. The worker owns everything that touches Messenger (a handle
//! belongs to the thread that made it): the shell's `os.lazy.notify` endpoint and the one event
//! channel (`Listen`) the shell keeps per app.
//!
//! The worker asks for the event channel before its first toast, polls it while any toast it
//! made is up (and sleeps when none is), and takes its toasts down when it is dropped: closing the
//! browser closes its notifications, which have nothing left to answer to.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Reason, ShellNews, Toasts};
use crate::transfers::Toast;

/// How often the event channel is looked at while a toast is up.
const POLL: Duration = Duration::from_millis(100);
/// How long a closing window spends taking its toasts down, at most.
const CLEAN_UP: Duration = Duration::from_millis(1500);

/// What happened to a toast, from the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Activated { toast: u64 },
    Closed { toast: u64, reason: Reason },
}

/// The shell, as the worker uses it: the platform's notification client (`xui_app::platform::notify`
/// on LazyOS).
pub trait Shell: Send + 'static {
    /// Shows `toast`; the shell's id for it. Asks for the event channel first when it has none.
    fn post(&mut self, toast: &Toast) -> Result<u64, String>;
    /// Takes a toast down.
    fn close(&mut self, toast: u64);
    /// What happened to the toasts since the last call.
    fn poll(&mut self) -> Vec<Event>;
}

enum Job {
    Post(u64, Toast),
    Close(u64),
}

/// The worker thread and the channels to it.
pub struct ShellWorker {
    jobs: Option<Sender<Job>>,
    news: Receiver<ShellNews>,
    thread: Option<JoinHandle<()>>,
}

impl ShellWorker {
    /// Starts the worker over `shell`. `wake` is called (from the worker's thread) whenever news
    /// is waiting: it must be cheap and thread-safe (a message to the window's event loop).
    pub fn spawn(shell: impl Shell, wake: impl Fn() + Send + 'static) -> Option<ShellWorker> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, news) = mpsc::channel::<ShellNews>();
        let thread = thread::Builder::new()
            .name(String::from("webnotify"))
            .spawn(move || run(shell, &inbox, &outbox, &wake))
            .ok()?;
        Some(ShellWorker {
            jobs: Some(jobs),
            news,
            thread: Some(thread),
        })
    }
}

impl Toasts for ShellWorker {
    fn post(&self, key: u64, toast: Toast) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Post(key, toast));
        }
    }

    fn close(&self, toast: u64) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Close(toast));
        }
    }

    fn drain(&self) -> Vec<ShellNews> {
        self.news.try_iter().collect()
    }
}

impl Drop for ShellWorker {
    fn drop(&mut self) {
        // The worker sees its inbox hang up, takes the toasts down and ends.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(mut shell: impl Shell, inbox: &Receiver<Job>, outbox: &Sender<ShellNews>, wake: &dyn Fn()) {
    // Toasts made and not yet reported closed.
    let mut open: HashSet<u64> = HashSet::new();
    loop {
        let job = if open.is_empty() {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            inbox.recv_timeout(POLL)
        };
        let mut news = Vec::new();
        match job {
            Ok(Job::Post(key, toast)) => match shell.post(&toast) {
                Ok(id) => {
                    open.insert(id);
                    news.push(ShellNews::Posted { key, toast: id });
                }
                Err(_) => news.push(ShellNews::Failed { key }),
            },
            Ok(Job::Close(toast)) => {
                if open.remove(&toast) {
                    shell.close(toast);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !open.is_empty() {
            for event in shell.poll() {
                match event {
                    Event::Activated { toast } => news.push(ShellNews::Activated { toast }),
                    Event::Closed { toast, reason } => {
                        open.remove(&toast);
                        news.push(ShellNews::Closed { toast, reason });
                    }
                }
            }
        }
        if !news.is_empty() {
            for item in news {
                let _ = outbox.send(item);
            }
            wake();
        }
    }
    let began = Instant::now();
    for toast in open {
        if began.elapsed() > CLEAN_UP {
            break;
        }
        shell.close(toast);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfers::ToastLevel;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Script {
        posted: Vec<String>,
        closed: Vec<u64>,
        events: Vec<Event>,
        refuse: bool,
        next: u64,
    }

    struct Fake(Arc<Mutex<Script>>);

    impl Shell for Fake {
        fn post(&mut self, toast: &Toast) -> Result<u64, String> {
            let mut script = self.0.lock().unwrap();
            if script.refuse {
                return Err(String::from("busy"));
            }
            script.next += 1;
            script.posted.push(toast.title.clone());
            Ok(script.next)
        }
        fn close(&mut self, toast: u64) {
            self.0.lock().unwrap().closed.push(toast);
        }
        fn poll(&mut self) -> Vec<Event> {
            std::mem::take(&mut self.0.lock().unwrap().events)
        }
    }

    fn toast(title: &str) -> Toast {
        Toast {
            level: ToastLevel::Info,
            title: title.into(),
            body: String::new(),
            sticky: false,
            replaces: None,
        }
    }

    /// Calls `look` until it returns something, or fails after two seconds.
    fn eventually<T>(mut look: impl FnMut() -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(found) = look() {
                return found;
            }
            assert!(Instant::now() < until, "nothing arrived");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_post_comes_back_with_the_shells_id_and_wakes_the_window() {
        let script = Arc::new(Mutex::new(Script::default()));
        let woken = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&woken);
        let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), move || {
            *counter.lock().unwrap() += 1;
        })
        .unwrap();
        worker.post(7, toast("a.test"));
        let news = eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        assert_eq!(news, vec![ShellNews::Posted { key: 7, toast: 1 }]);
        assert!(*woken.lock().unwrap() >= 1);
        assert_eq!(script.lock().unwrap().posted, ["a.test"]);
    }

    #[test]
    fn a_refused_post_is_reported_as_failed() {
        let script = Arc::new(Mutex::new(Script {
            refuse: true,
            ..Script::default()
        }));
        let worker = ShellWorker::spawn(Fake(script), || {}).unwrap();
        worker.post(3, toast("a.test"));
        let news = eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        assert_eq!(news, vec![ShellNews::Failed { key: 3 }]);
    }

    #[test]
    fn clicks_and_closes_reach_the_window_while_a_toast_is_up() {
        let script = Arc::new(Mutex::new(Script::default()));
        let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), || {}).unwrap();
        worker.post(1, toast("a.test"));
        eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        script.lock().unwrap().events = vec![
            Event::Activated { toast: 1 },
            Event::Closed {
                toast: 1,
                reason: Reason::User,
            },
        ];
        let news = eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        assert_eq!(
            news,
            vec![
                ShellNews::Activated { toast: 1 },
                ShellNews::Closed {
                    toast: 1,
                    reason: Reason::User
                }
            ]
        );
    }

    #[test]
    fn closing_the_window_takes_its_toasts_down_and_only_those_still_up() {
        let script = Arc::new(Mutex::new(Script::default()));
        {
            let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), || {}).unwrap();
            for key in 1..=3 {
                worker.post(key, toast("a.test"));
            }
            let mut seen = 0;
            eventually(|| {
                seen += worker.drain().len();
                (seen == 3).then_some(())
            });
            // Toast 2 was dismissed by the person.
            script.lock().unwrap().events = vec![Event::Closed {
                toast: 2,
                reason: Reason::Timeout,
            }];
            eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        }
        let mut closed = script.lock().unwrap().closed.clone();
        closed.sort_unstable();
        assert_eq!(closed, vec![1, 3]);
    }

    #[test]
    fn a_page_that_took_its_toast_down_closes_it_once() {
        let script = Arc::new(Mutex::new(Script::default()));
        let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), || {}).unwrap();
        worker.post(1, toast("a.test"));
        eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        worker.close(1);
        eventually(|| (script.lock().unwrap().closed == [1]).then_some(()));
        drop(worker);
        assert_eq!(
            script.lock().unwrap().closed,
            [1],
            "not closed again at the end"
        );
    }
}

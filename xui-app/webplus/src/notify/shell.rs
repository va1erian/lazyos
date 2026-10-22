//! The window's line to the shell, on a thread of its own.
//!
//! A toast is best effort and the shell answers from its heartbeat, so the window never waits for
//! one: it hands the request to this worker and collects what came back (the toast's id, a click,
//! a close) when the worker wakes it. The worker owns everything that touches Messenger (a handle
//! belongs to the thread that made it): the shell's `os.lazy.notify` endpoint and the one event
//! channel (`Listen`) the shell keeps per app.
//!
//! The worker asks for the event channel before its first toast and, while any toast it made is
//! up, waits for the channel to have news (issue #893: never a timer that wakes the busy main
//! thread); with none up it sleeps on its inbox. It takes its toasts down when it is dropped:
//! closing the browser closes its notifications, which have nothing left to answer to.
//!
//! Only a toast posted while the event channel was there is waited for: one posted without it
//! (the shell refused `Listen`) can never be heard of. When the channel is lost (the shell
//! restarted, and its toasts with it), every toast still open is reported closed, so a lost
//! `Closed` cannot keep the worker waiting for ever.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Reason, ShellNews, Toasts};
use crate::transfers::Toast;

/// The longest the worker waits for the shell's news before it looks at its inbox again, while a
/// toast is up: how late a post or a close can be then. The wait itself is on this thread only.
const WAIT: Duration = Duration::from_millis(100);
/// How long a closing window spends taking its toasts down, at most.
const CLEAN_UP: Duration = Duration::from_millis(1500);

/// What happened to a toast, from the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Activated {
        toast: u64,
    },
    Closed {
        toast: u64,
        reason: Reason,
    },
    /// Event channel `channel` is gone (the shell restarted): nothing more will be heard of the
    /// toasts posted while it was there, and the shell no longer shows them. Toasts posted on a
    /// newer channel are not concerned.
    Lost {
        channel: u64,
    },
}

/// A toast the shell took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posted {
    /// The shell's id for it.
    pub toast: u64,
    /// The event channel that was there when it was posted (each new one has a higher number), so
    /// its click and its close will be heard; `None` without one.
    pub heard: Option<u64>,
}

/// The shell, as the worker uses it: the platform's notification client (`xui_app::platform::notify`
/// on LazyOS).
pub trait Shell: Send + 'static {
    /// Shows `toast`. Asks for the event channel first when it has none.
    fn post(&mut self, toast: &Toast) -> Result<Posted, String>;
    /// Takes a toast down.
    fn close(&mut self, toast: u64);
    /// Parks the worker until the event channel may have news, or `timeout` passes.
    fn wait(&mut self, timeout: Duration);
    /// What happened to the toasts since the last call; cheap when [`Shell::wait`] found nothing.
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
    /// Run while the worker is being joined: the shell's calls are made on the window's thread
    /// (`platform::main_thread`), which must keep taking them while the worker takes its toasts down.
    turn: Option<Box<dyn Fn()>>,
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
            turn: None,
        })
    }

    /// `turn` is run, over and over, while a dropped worker ends (see the field).
    pub fn with_turn(mut self, turn: impl Fn() + 'static) -> ShellWorker {
        self.turn = Some(Box::new(turn));
        self
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
            if let Some(turn) = &self.turn {
                // The worker takes its toasts down within CLEAN_UP; a worker stuck past twice that is
                // left behind (it ends with the process) rather than holding the quit.
                let until = Instant::now() + CLEAN_UP * 2;
                while !thread.is_finished() && Instant::now() < until {
                    turn();
                    thread::sleep(Duration::from_millis(2));
                }
                if !thread.is_finished() {
                    return;
                }
            }
            let _ = thread.join();
        }
    }
}

fn run(mut shell: impl Shell, inbox: &Receiver<Job>, outbox: &Sender<ShellNews>, wake: &dyn Fn()) {
    // Toasts made and not yet reported closed, with the event channel that will tell of them.
    let mut open: HashMap<u64, u64> = HashMap::new();
    loop {
        let job = if open.is_empty() {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            next_job(&mut shell, inbox)
        };
        let mut news = Vec::new();
        match job {
            Ok(Job::Post(key, toast)) => match shell.post(&toast) {
                Ok(posted) => {
                    if let Some(channel) = posted.heard {
                        open.insert(posted.toast, channel);
                    }
                    news.push(ShellNews::Posted {
                        key,
                        toast: posted.toast,
                    });
                }
                Err(_) => news.push(ShellNews::Failed { key }),
            },
            Ok(Job::Close(toast)) => {
                if open.remove(&toast).is_some() {
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
                    Event::Lost { channel } => {
                        let gone: Vec<u64> = open
                            .iter()
                            .filter(|(_, heard)| **heard == channel)
                            .map(|(toast, _)| *toast)
                            .collect();
                        for toast in gone {
                            open.remove(&toast);
                            news.push(ShellNews::Closed {
                                toast,
                                reason: Reason::Restart,
                            });
                        }
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
    for toast in open.into_keys() {
        if began.elapsed() > CLEAN_UP {
            break;
        }
        shell.close(toast);
    }
}

/// The next job while a toast is up: one already waiting, else one that came while the worker
/// waited (at most [`WAIT`]) for the shell's news. A timeout means "look at the shell".
fn next_job(shell: &mut impl Shell, inbox: &Receiver<Job>) -> Result<Job, RecvTimeoutError> {
    match inbox.try_recv() {
        Ok(job) => return Ok(job),
        Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
        Err(TryRecvError::Empty) => {}
    }
    shell.wait(WAIT);
    inbox.try_recv().map_err(|error| match error {
        TryRecvError::Empty => RecvTimeoutError::Timeout,
        TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
    })
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
        /// Posts are made with no event channel.
        deaf: bool,
        /// The event channel posts are heard on.
        channel: u64,
        next: u64,
        waits: u32,
    }

    struct Fake(Arc<Mutex<Script>>);

    impl Shell for Fake {
        fn post(&mut self, toast: &Toast) -> Result<Posted, String> {
            let mut script = self.0.lock().unwrap();
            if script.refuse {
                return Err(String::from("busy"));
            }
            script.next += 1;
            script.posted.push(toast.title.clone());
            Ok(Posted {
                toast: script.next,
                heard: (!script.deaf).then_some(script.channel),
            })
        }
        fn close(&mut self, toast: u64) {
            self.0.lock().unwrap().closed.push(toast);
        }
        fn wait(&mut self, timeout: Duration) {
            self.0.lock().unwrap().waits += 1;
            thread::sleep(timeout.min(Duration::from_millis(5)));
        }
        fn poll(&mut self) -> Vec<Event> {
            std::mem::take(&mut self.0.lock().unwrap().events)
        }
    }

    /// Whether the worker stopped waiting on the shell: its count of waits stays still.
    fn settled(script: &Arc<Mutex<Script>>) -> bool {
        let before = script.lock().unwrap().waits;
        thread::sleep(Duration::from_millis(60));
        script.lock().unwrap().waits == before
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
    fn a_lost_event_channel_closes_every_open_toast_and_the_waiting_stops() {
        let script = Arc::new(Mutex::new(Script::default()));
        let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), || {}).unwrap();
        worker.post(1, toast("a.test"));
        worker.post(2, toast("b.test"));
        let mut seen = 0;
        eventually(|| {
            seen += worker.drain().len();
            (seen == 2).then_some(())
        });
        assert!(!settled(&script), "waits for news while toasts are up");
        // A toast posted on the shell's next channel is not the old channel's to close.
        script.lock().unwrap().channel = 1;
        worker.post(3, toast("c.test"));
        eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        script.lock().unwrap().events = vec![Event::Lost { channel: 0 }];
        let mut news = Vec::new();
        eventually(|| {
            news.extend(worker.drain());
            (news.len() == 2).then_some(())
        });
        news.sort_by_key(|n| match n {
            ShellNews::Closed { toast, .. } => *toast,
            _ => u64::MAX,
        });
        assert_eq!(
            news,
            vec![
                ShellNews::Closed {
                    toast: 1,
                    reason: Reason::Restart
                },
                ShellNews::Closed {
                    toast: 2,
                    reason: Reason::Restart
                }
            ]
        );
        assert!(!settled(&script), "toast 3 is still waited for");
        script.lock().unwrap().events = vec![Event::Lost { channel: 1 }];
        let news = eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        assert_eq!(
            news,
            vec![ShellNews::Closed {
                toast: 3,
                reason: Reason::Restart
            }]
        );
        assert!(settled(&script), "nothing left to wait for");
    }

    #[test]
    fn a_toast_posted_without_the_event_channel_is_not_waited_for() {
        let script = Arc::new(Mutex::new(Script {
            deaf: true,
            ..Script::default()
        }));
        let worker = ShellWorker::spawn(Fake(Arc::clone(&script)), || {}).unwrap();
        worker.post(1, toast("a.test"));
        let news = eventually(|| Some(worker.drain()).filter(|n| !n.is_empty()));
        assert_eq!(news, vec![ShellNews::Posted { key: 1, toast: 1 }]);
        assert!(settled(&script));
        assert_eq!(script.lock().unwrap().waits, 0);
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

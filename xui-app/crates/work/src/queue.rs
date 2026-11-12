//! One long-lived worker that serves requests in order.

use std::io;
use std::sync::mpsc::{channel, Sender};

use crate::sink::Mailbox;

/// A worker thread with a request queue: each request goes through `handle`
/// on the thread and its answer is posted as a message. For work that arrives
/// as a stream (a document loader, a folder scanner) where a thread per
/// request would pile up; one-shot work uses [`spawn`](crate::spawn).
///
/// The thread ends when the `Queue` is dropped or the window is gone. A
/// handler that panics ends the thread; the next [`send`](Queue::send)
/// returns `false`, which is how the owner learns the worker is gone.
pub struct Queue<Req> {
    requests: Sender<Req>,
}

impl<Req: Send + 'static> Queue<Req> {
    /// Starts the worker; every request's answer is posted to `to`.
    pub fn start<M, B>(
        to: &B,
        name: &str,
        mut handle: impl FnMut(Req) -> M + Send + 'static,
    ) -> io::Result<Queue<Req>>
    where
        B: Mailbox<M>,
        M: 'static,
    {
        let (requests, inbox) = channel::<Req>();
        let mailbox = to.clone();
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Ok(request) = inbox.recv() {
                    if !mailbox.post(handle(request)) {
                        return;
                    }
                }
            })?;
        Ok(Queue { requests })
    }

    /// Queues `request`; `false` when the worker is gone.
    pub fn send(&self, request: Req) -> bool {
        self.requests.send(request).is_ok()
    }
}

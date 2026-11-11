//! Where a worker posts: a window's proxy, or a [`Sink`] over one.

use std::sync::Arc;

use xui_core::app::Proxy;

/// Somewhere a worker can post a message: a window's [`Proxy`] or a [`Sink`].
pub trait Mailbox<M>: Clone + Send + 'static {
    /// Posts `msg`; `false` once the window is gone.
    fn post(&self, msg: M) -> bool;
}

impl<M: Send + 'static> Mailbox<M> for Proxy<M> {
    fn post(&self, msg: M) -> bool {
        self.send(msg).is_ok()
    }
}

/// A handle that posts values of type `R`, turning each into the app's
/// message on the way: how a crate that does not name the app's message type
/// (a pool of render threads, a thumbnail doorbell) reaches its window. Cheap
/// to clone; usable from any thread.
pub struct Sink<R>(Arc<dyn Fn(R) -> bool + Send + Sync>);

impl<R> Clone for Sink<R> {
    fn clone(&self) -> Sink<R> {
        Sink(Arc::clone(&self.0))
    }
}

impl<R: 'static> Sink<R> {
    /// Posts each value through `proxy` as `wrap(value)`.
    pub fn new<M: Send + 'static>(
        proxy: &Proxy<M>,
        wrap: impl Fn(R) -> M + Send + Sync + 'static,
    ) -> Sink<R> {
        let proxy = proxy.clone();
        Sink::from_fn(move |value| proxy.send(wrap(value)).is_ok())
    }

    /// A sink over any delivery function (host tests collect with one);
    /// `false` from it means the receiver is gone.
    pub fn from_fn(post: impl Fn(R) -> bool + Send + Sync + 'static) -> Sink<R> {
        Sink(Arc::new(post))
    }

    /// Posts `value`; `false` once the window is gone.
    pub fn send(&self, value: R) -> bool {
        (self.0)(value)
    }
}

impl Sink<()> {
    /// A doorbell: ringing posts `msg`. A burst of rings before the UI thread
    /// collects posts one wake (the proxy coalesces them).
    pub fn bell<M: Clone + Send + Sync + 'static>(proxy: &Proxy<M>, msg: M) -> Sink<()> {
        Sink::new(proxy, move |()| msg.clone())
    }

    /// Rings the doorbell.
    pub fn ring(&self) -> bool {
        self.send(())
    }
}

impl<R: 'static> Mailbox<R> for Sink<R> {
    fn post(&self, msg: R) -> bool {
        self.send(msg)
    }
}

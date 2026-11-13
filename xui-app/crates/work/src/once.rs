//! A value in a message that is not `Clone`, taken once.

use std::fmt;
use std::sync::{Arc, Mutex};

/// A result a worker posts as (part of) a message: xui messages are `Clone`,
/// a result often is not. Every clone sees the same slot, and the first
/// [`take`](Once::take) empties it.
pub struct Once<T>(Arc<Mutex<Option<T>>>);

impl<T> Once<T> {
    /// A slot holding `value`.
    pub fn new(value: T) -> Once<T> {
        Once(Arc::new(Mutex::new(Some(value))))
    }

    /// The value (once; `None` after, for this and every clone).
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl<T> Clone for Once<T> {
    fn clone(&self) -> Self {
        Once(Arc::clone(&self.0))
    }
}

impl<T> fmt::Debug for Once<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Once(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_take_of_any_clone_gets_it() {
        let once = Once::new(String::from("result"));
        let copy = once.clone();
        assert_eq!(copy.take().as_deref(), Some("result"));
        assert_eq!(once.take(), None);
    }
}

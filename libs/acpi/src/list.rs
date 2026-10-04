//! A fixed-capacity list, so the parsers need no allocator.

/// Up to `N` copies of `T`; pushes past the capacity are counted, not stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct List<T: Copy + Default, const N: usize> {
    items: [T; N],
    len: usize,
    dropped: usize,
}

impl<T: Copy + Default, const N: usize> Default for List<T, N> {
    fn default() -> Self {
        List {
            items: [T::default(); N],
            len: 0,
            dropped: 0,
        }
    }
}

impl<T: Copy + Default, const N: usize> List<T, N> {
    /// Append `item`; returns false (and counts it as dropped) when full.
    pub fn push(&mut self, item: T) -> bool {
        if self.len == N {
            self.dropped += 1;
            return false;
        }
        self.items[self.len] = item;
        self.len += 1;
        true
    }

    pub fn as_slice(&self) -> &[T] {
        &self.items[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Items that did not fit.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

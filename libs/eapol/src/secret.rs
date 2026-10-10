//! A fixed-size secret that does not print itself and wipes on drop.

/// `N` octets of key material. `Debug` is redacted; the octets are
/// overwritten when the value drops (best effort: with no `unsafe` the
/// compiler is only discouraged from removing the writes, via `black_box`).
#[derive(Clone)]
#[cfg_attr(any(test, feature = "fuzz"), derive(PartialEq, Eq))]
pub struct Key<const N: usize>([u8; N]);

impl<const N: usize> Key<N> {
    pub const fn new(bytes: [u8; N]) -> Key<N> {
        Key(bytes)
    }

    pub fn expose(&self) -> &[u8; N] {
        &self.0
    }

    pub fn expose_mut(&mut self) -> &mut [u8; N] {
        &mut self.0
    }
}

impl<const N: usize> Key<N> {
    /// A copy of `N` octets starting at `at` of a longer key (the PTK's
    /// KCK, KEK and TK). The caller keeps `at + N <= M`.
    pub fn slice_of<const M: usize>(parent: &Key<M>, at: usize) -> Key<N> {
        let mut out = [0u8; N];
        out.copy_from_slice(&parent.0[at..at + N]);
        Key(out)
    }
}

impl<const N: usize> core::fmt::Debug for Key<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Key<{N}>(..)")
    }
}

impl<const N: usize> Drop for Key<N> {
    fn drop(&mut self) {
        self.0.fill(0);
        core::hint::black_box(&mut self.0);
    }
}

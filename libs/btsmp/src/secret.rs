//! A fixed-size secret that never prints and zeroes itself on drop.

use zeroize::Zeroize;

/// Key material (an LTK or an IRK). `Debug` is written by hand so no trace,
/// log line or `assert_eq!` failure can carry the bytes; read them with
/// [`Secret::expose`] at the one place that needs them (the bond store, the
/// HCI command that starts encryption).
#[derive(Clone)]
pub struct Secret<const N: usize>([u8; N]);

impl<const N: usize> Secret<N> {
    /// Wrap `bytes`.
    pub fn new(bytes: [u8; N]) -> Secret<N> {
        Secret(bytes)
    }

    /// The bytes, for the caller that must store or use the key.
    pub fn expose(&self) -> &[u8; N] {
        &self.0
    }
}

impl<const N: usize> core::fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Constant-time equality, so a comparison of two keys leaks no prefix length.
impl<const N: usize> PartialEq for Secret<N> {
    fn eq(&self, other: &Self) -> bool {
        lazyos_crypto::wifi::mic_eq(&self.0, &other.0)
    }
}

impl<const N: usize> Eq for Secret<N> {}

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

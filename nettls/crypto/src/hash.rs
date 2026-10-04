//! SHA-256 and SHA-384 as rustls hashes.

use rustls::crypto::hash::{self, HashAlgorithm};
use sha2::Digest;

/// A rustls hash over any `sha2` digest.
pub struct Hash<D>(std::marker::PhantomData<fn() -> D>, HashAlgorithm);

pub static SHA256: Hash<sha2::Sha256> = Hash(std::marker::PhantomData, HashAlgorithm::SHA256);
pub static SHA384: Hash<sha2::Sha384> = Hash(std::marker::PhantomData, HashAlgorithm::SHA384);

impl<D: Digest + Clone + Send + Sync + 'static> hash::Hash for Hash<D> {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Context(D::new()))
    }

    fn hash(&self, data: &[u8]) -> hash::Output {
        hash::Output::new(&D::digest(data))
    }

    fn output_len(&self) -> usize {
        <D as Digest>::output_size()
    }

    fn algorithm(&self) -> HashAlgorithm {
        self.1
    }
}

struct Context<D>(D);

impl<D: Digest + Clone + Send + Sync + 'static> hash::Context for Context<D> {
    fn fork_finish(&self) -> hash::Output {
        hash::Output::new(&self.0.clone().finalize())
    }

    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Context(self.0.clone()))
    }

    fn finish(self: Box<Self>) -> hash::Output {
        hash::Output::new(&self.0.finalize())
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

#[cfg(test)]
mod tests {
    use rustls::crypto::hash::Hash as _;

    #[test]
    fn known_answers() {
        // FIPS 180-2 "abc" vectors.
        let h = super::SHA256.hash(b"abc");
        assert_eq!(h.as_ref()[..4], [0xba, 0x78, 0x16, 0xbf],);
        let mut ctx = super::SHA384.start();
        ctx.update(b"a");
        let fork = ctx.fork();
        ctx.update(b"bc");
        assert_eq!(ctx.finish().as_ref()[..4], [0xcb, 0x00, 0x75, 0x3f]);
        assert_eq!(
            fork.fork_finish().as_ref(),
            super::SHA384.hash(b"a").as_ref()
        );
    }
}

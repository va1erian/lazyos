//! Which channels print: `trace.mask=init,pkgd,-shell` in `lazyos.cfg`.
//!
//! The text is untrusted boot input in the `limits.rs` style: tolerant, every
//! problem is reported to the caller to log, nothing is fatal.
//!
//! Tokens are comma separated and apply left to right:
//!
//! * `all` every channel, `none` no channel, `default` the channels that are
//!   on without being asked for ([`Channel::default_on`]);
//! * `name` adds a channel, `-name` removes one (names are lower-case
//!   prefixes: `init`, `pkgd`, `ui`).
//!
//! A list that starts with a removal (`-shell`) starts from `default`, so
//! `-shell` alone means "the usual, without the shell"; any other list starts
//! empty, so `init,pkgd` means exactly those two.

use crate::channel::MASK_BITS;
use crate::Channel;

const WORDS: usize = MASK_BITS / 64;

/// A set of channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask {
    words: [u64; WORDS],
}

/// Something in a mask text that was ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem<'a> {
    /// A name that is no channel.
    Unknown(&'a str),
    /// An empty token (`init,,pkgd`).
    Empty,
}

impl Mask {
    pub const NONE: Mask = Mask { words: [0; WORDS] };

    /// Every channel in the catalogue.
    pub const fn all() -> Mask {
        let mut mask = Mask::NONE;
        let mut index = 0;
        while index < Channel::ALL.len() {
            mask.words[index / 64] |= 1 << (index % 64);
            index += 1;
        }
        mask
    }

    /// The channels that are on unless a build asks otherwise.
    pub fn defaults() -> Mask {
        let mut mask = Mask::NONE;
        for channel in Channel::ALL {
            if channel.default_on() {
                mask.set(*channel, true);
            }
        }
        mask
    }

    pub const fn from_words(words: [u64; WORDS]) -> Mask {
        Mask { words }
    }

    pub const fn words(&self) -> [u64; WORDS] {
        self.words
    }

    pub const fn allows(&self, channel: Channel) -> bool {
        let index = channel.index();
        self.words[index / 64] >> (index % 64) & 1 == 1
    }

    pub fn set(&mut self, channel: Channel, on: bool) {
        let index = channel.index();
        let bit = 1u64 << (index % 64);
        if on {
            self.words[index / 64] |= bit;
        } else {
            self.words[index / 64] &= !bit;
        }
    }

    /// The mask `text` spells; each ignored token goes to `problem`.
    pub fn parse(text: &str, mut problem: impl FnMut(Problem<'_>)) -> Mask {
        let text = text.split('#').next().unwrap_or("").trim();
        let starts_with_removal = text.trim_start().starts_with('-');
        let mut mask = if starts_with_removal {
            Mask::defaults()
        } else {
            Mask::NONE
        };
        for token in text.split(',') {
            let token = token.trim();
            match token {
                "" => problem(Problem::Empty),
                "all" => mask = Mask::all(),
                "none" => mask = Mask::NONE,
                "default" => mask = Mask::defaults(),
                _ => {
                    let (on, name) = match token.strip_prefix('-') {
                        Some(name) => (false, name.trim()),
                        None => (true, token),
                    };
                    match Channel::from_name(name) {
                        Some(channel) => mask.set(channel, on),
                        None => problem(Problem::Unknown(name)),
                    }
                }
            }
        }
        mask
    }
}

impl Default for Mask {
    fn default() -> Self {
        Mask::defaults()
    }
}

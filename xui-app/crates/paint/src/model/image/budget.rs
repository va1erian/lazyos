#![forbid(unsafe_code)]

//! The memory budget: documents are bounded by the bytes they hold, not by a
//! small side limit.
//!
//! A document needs `layers + the flattened cache` (each `width x height x 4`),
//! plus whatever else is reserved: a floating or clipboard bitmap and the
//! history's own cap. The window-sized screen image is not counted.

use std::fmt;

/// The most memory the document, its flattened cache, a floating bitmap and
/// the history may hold together (512 MiB). A 1 GiB guest also needs room for
/// the rest of the desktop.
pub const MEMORY_BUDGET: u64 = 512 * 1024 * 1024;

/// The bytes of `layers` layers plus the flattened cache for a `width` x
/// `height` document.
pub fn required_bytes(width: u32, height: u32, layers: usize) -> u64 {
    u64::from(width) * u64::from(height) * 4 * (layers as u64 + 1)
}

/// Memory held besides the document and its cache, which every allocation
/// path must leave room for. Cut, copy, paste and Move Selected Pixels (P2)
/// fill in `floating` and `clipboard`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reserved {
    /// The floating selection's bitmap.
    pub floating: u64,
    /// The app-wide clipboard bitmap.
    pub clipboard: u64,
    /// The history's byte cap.
    pub history: u64,
}

impl Reserved {
    /// The bytes reserved in all.
    pub fn total(&self) -> u64 {
        self.floating
            .saturating_add(self.clipboard)
            .saturating_add(self.history)
    }
}

/// A document, resize or new layer that would not fit the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetError {
    /// The requested width.
    pub width: u32,
    /// The requested height.
    pub height: u32,
    /// The layer count the request would hold.
    pub layers: usize,
    /// What else was held.
    pub reserved: Reserved,
}

const MIB: u64 = 1024 * 1024;

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let needed = required_bytes(self.width, self.height, self.layers) + self.reserved.total();
        write!(
            f,
            "not enough memory: {} x {} with {} layer{} needs {} MiB (with the picture cache and \
             {} MiB reserved for undo), the limit is {} MiB",
            self.width,
            self.height,
            self.layers,
            if self.layers == 1 { "" } else { "s" },
            needed.div_ceil(MIB),
            self.reserved.history.div_ceil(MIB),
            MEMORY_BUDGET / MIB,
        )
    }
}

impl std::error::Error for BudgetError {}

/// Checks that a `width` x `height` document of `layers` layers, its cache and
/// the `reserved` bytes (`layers + cache + floating + clipboard + history`) fit.
pub fn check(
    width: u32,
    height: u32,
    layers: usize,
    reserved: Reserved,
) -> Result<(), BudgetError> {
    if required_bytes(width, height, layers).saturating_add(reserved.total()) > MEMORY_BUDGET {
        Err(BudgetError {
            width,
            height,
            layers,
            reserved,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(floating: u64, clipboard: u64) -> Reserved {
        Reserved {
            floating,
            clipboard,
            history: 64 * MIB,
        }
    }

    #[test]
    fn the_budget_counts_layers_cache_and_reserve() {
        // 8192 x 4096 is 128 MiB a layer; layers + cache with 64 MiB reserved
        // leaves 448 MiB, so two layers (3 x 128) fit and three (4 x 128) do not.
        let reserve = with(0, 0);
        assert!(check(8192, 4096, 2, reserve).is_ok());
        assert!(check(8192, 4096, 3, reserve).is_err());
        assert!(
            check(8192, 8192, 1, reserve).is_err(),
            "layer + cache + undo"
        );
        assert!(check(4096, 8192, 1, reserve).is_ok());
        assert!(check(16384, 16384, 1, Reserved::default()).is_err());
        // The floating and clipboard bitmaps each shrink what is left.
        assert!(check(4096, 8192, 1, with(96 * MIB, 96 * MIB)).is_ok());
        assert!(check(4096, 8192, 1, with(96 * MIB, 97 * MIB)).is_err());
        assert!(check(4096, 8192, 1, with(97 * MIB, 96 * MIB)).is_err());
    }

    #[test]
    fn the_message_names_the_numbers() {
        let text = check(16384, 16384, 1, with(0, 0)).unwrap_err().to_string();
        assert!(text.contains("16384 x 16384"), "{text}");
        assert!(text.contains("2112 MiB"), "{text}");
        assert!(text.contains("512 MiB"), "{text}");
    }
}

//! What a conversion kept and what it dropped, so the browser can tell the user
//! (`about:blocker`) and a test can pin it.

use std::fmt;

/// Why a filter did not become a rule. A dropped filter is never approximated:
/// a rule that blocks more than the list meant breaks pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// Element hiding (`##`, `#@#`, `#?#`, ...) and the `$elemhide` family: v1 blocks
    /// requests only.
    Cosmetic,
    /// An option WebKit has no trigger for (`$csp`, `$redirect`, `$removeparam`, `$to`,
    /// `$denyallow`, a domain with a wildcard, ...).
    UnsupportedOption,
    /// A pattern outside WebKit's URL-filter subset (alternation, `\b`, `{n,m}`, ...).
    UnsupportedPattern,
    /// Not ASCII: WebKit's URL filters are ASCII only.
    NonAscii,
    /// `$domain=` with both included and excluded domains: one trigger holds one condition.
    MixedDomains,
    /// More exceptions than a list may hold next to its blocks.
    OverLimit,
}

impl DropReason {
    /// Every reason, in the order `Stats` stores them.
    pub const ALL: [DropReason; 6] = [
        DropReason::Cosmetic,
        DropReason::UnsupportedOption,
        DropReason::UnsupportedPattern,
        DropReason::NonAscii,
        DropReason::MixedDomains,
        DropReason::OverLimit,
    ];

    /// A short stable name for markers and `about:blocker`.
    pub fn name(self) -> &'static str {
        match self {
            DropReason::Cosmetic => "cosmetic",
            DropReason::UnsupportedOption => "unsupported-option",
            DropReason::UnsupportedPattern => "unsupported-pattern",
            DropReason::NonAscii => "non-ascii",
            DropReason::MixedDomains => "mixed-domains",
            DropReason::OverLimit => "over-limit",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Counts for one conversion.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Lines read.
    pub lines: usize,
    /// Blank lines, `!` comments and the `[Adblock Plus ...]` header.
    pub comments: usize,
    /// Rules that block (without `$important`), after removing duplicates.
    pub blocks: usize,
    /// `@@` exceptions, which become `ignore-previous-rules`.
    pub exceptions: usize,
    /// `$important` blocks (placed after the exceptions so they cannot be cancelled).
    pub important: usize,
    /// Rules identical to an earlier one.
    pub duplicates: usize,
    /// Filters removed by a `$badfilter` line.
    pub badfiltered: usize,
    /// Of the dropped filters, how many were exceptions (`@@`): the block they would have cancelled
    /// stays, which is the one way a drop makes a list block *more*.
    pub dropped_exceptions: usize,
    /// Selectors of element-hiding lines that became `css-display-none` rules (several to a rule).
    pub hides: usize,
    dropped: [usize; 6],
}

impl Stats {
    /// How many filters were dropped for `reason`.
    pub fn dropped(&self, reason: DropReason) -> usize {
        self.dropped[reason.index()]
    }

    /// Every dropped filter.
    pub fn dropped_total(&self) -> usize {
        self.dropped.iter().sum()
    }

    pub(crate) fn drop_filter(&mut self, reason: DropReason) {
        self.dropped[reason.index()] += 1;
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lines={} blocks={} exceptions={} important={} hides={} duplicates={} badfiltered={}",
            self.lines,
            self.blocks,
            self.exceptions,
            self.important,
            self.hides,
            self.duplicates,
            self.badfiltered
        )?;
        if self.dropped_exceptions > 0 {
            write!(f, " dropped_exceptions={}", self.dropped_exceptions)?;
        }
        for reason in DropReason::ALL {
            let n = self.dropped(reason);
            if n > 0 {
                write!(f, " dropped:{}={}", reason.name(), n)?;
            }
        }
        Ok(())
    }
}

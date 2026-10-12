//! Find: plain-text search over the scrollback and the screen.
//!
//! A [`Search`] is incremental. [`Search::update`] scans a bounded number of
//! rows per call (so a keystroke never stalls the UI with 10k lines of
//! history) and reports whether it is finished; the host calls it again from
//! a timer while it is not. Matches may span wrapped rows, are numbered by
//! absolute line like selections, and never overlap.
//!
//! The history does not change under a search except by eviction, a clear or a
//! reflow, so its matches are kept: those on evicted lines are dropped, a clear
//! or a reflow (`invalidate`) starts over. The screen does change with every
//! byte of output, so the logical lines that touch it are rescanned on each
//! update.

use crate::logical::{read, start_of};
use crate::{Point, Term};

/// The longest needle, in characters.
pub const MAX_NEEDLE: usize = 256;
/// The most matches kept (a search for "e" in a big scrollback stops here).
pub const MAX_MATCHES: usize = 50_000;
/// Rows scanned per [`Search::update`] when the host asks for the default.
pub const DEFAULT_BUDGET: usize = 4000;

/// One match: its first and last cells (inclusive; a wide character's last
/// cell is its second).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub start: Point,
    pub end: Point,
}

pub struct Search {
    needle: Vec<char>,
    case_sensitive: bool,
    matches: Vec<Match>,
    /// Every logical line starting before this row has been scanned.
    scanned: u64,
    current: Option<usize>,
    /// The match limit was hit.
    truncated: bool,
    /// The scan has reached the end of the screen.
    done: bool,
}

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl Search {
    /// A search for `needle` (an empty or over-long one finds nothing).
    pub fn new(needle: &str, case_sensitive: bool) -> Search {
        let needle: Vec<char> = needle.chars().take(MAX_NEEDLE).map(|c| fold(c, case_sensitive)).collect();
        Search {
            needle,
            case_sensitive,
            matches: Vec::new(),
            scanned: 0,
            current: None,
            truncated: false,
            done: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Whether the scan has covered everything (the count is final until the
    /// terminal's content changes).
    pub fn done(&self) -> bool {
        self.done
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn count(&self) -> usize {
        self.matches.len()
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// The match selected (Enter and Shift+Enter step it), if any.
    pub fn current(&self) -> Option<Match> {
        self.current.and_then(|i| self.matches.get(i)).copied()
    }

    /// 1-based index of the current match.
    pub fn current_index(&self) -> Option<usize> {
        self.current.map(|i| i + 1)
    }

    /// Start over: the terminal's lines were renumbered or cleared.
    pub fn invalidate(&mut self) {
        self.matches.clear();
        self.scanned = 0;
        self.current = None;
        self.truncated = false;
        self.done = false;
    }

    /// Scan up to `budget` more rows; returns whether the search is complete.
    /// Call again (from a timer) while it is not. `anchor` is the first row of
    /// the view: the first selection of a match prefers the one at or after it.
    pub fn update(&mut self, term: &Term, budget: usize, anchor: u64) -> bool {
        if self.needle.is_empty() {
            self.matches.clear();
            self.current = None;
            self.done = true;
            return true;
        }
        let first = term.first_line();
        // Evicted lines take their matches with them.
        let evicted = self.matches.partition_point(|m| m.start.line < first);
        if evicted > 0 {
            self.matches.drain(..evicted);
            self.current = self.current.and_then(|c| c.checked_sub(evicted));
        }
        self.scanned = self.scanned.max(first);
        // The stable history ends at the first row of the logical line that
        // reaches the screen; everything before it is scanned once.
        let screen = term.screen_top();
        let stable = start_of(term, screen).max(first);
        if self.scanned > stable {
            // The history shrank (a clear, a reflow): start over.
            self.invalidate();
            self.scanned = first;
        }
        // Matches past the stable part were found on the screen: rescanned.
        let keep = self.current().map(|m| m.start);
        let volatile = self.matches.partition_point(|m| m.start.line < self.scanned);
        if volatile < self.matches.len() {
            self.matches.truncate(volatile);
            self.current = None;
        }
        let mut rows = 0;
        let mut at = self.scanned;
        while at < stable && rows < budget {
            let Some(logical) = read(term, at) else { break };
            rows += (logical.last - logical.first + 1) as usize;
            self.scan_line(&logical);
            at = logical.last + 1;
        }
        self.scanned = at.min(stable);
        if self.scanned < stable {
            self.done = false;
            return false;
        }
        // The volatile part: the screen's logical lines, every time.
        let cut = self.matches.partition_point(|m| m.start.line < stable);
        self.matches.truncate(cut);
        let mut at = stable;
        let end = screen + term.rows() as u64;
        while at < end {
            let Some(logical) = read(term, at) else { break };
            self.scan_line(&logical);
            at = logical.last + 1;
        }
        self.done = true;
        self.settle(keep, anchor);
        true
    }

    /// Find the needle in one logical line.
    fn scan_line(&mut self, logical: &crate::logical::Logical) {
        let n = self.needle.len();
        let chars = &logical.chars;
        let mut i = 0;
        while i + n <= chars.len() {
            if self.matches.len() >= MAX_MATCHES {
                self.truncated = true;
                return;
            }
            if (0..n).all(|k| fold(chars[i + k].ch, self.case_sensitive) == self.needle[k]) {
                let (a, b) = (chars[i], chars[i + n - 1]);
                self.matches.push(Match {
                    start: Point::new(a.line, a.col),
                    end: Point::new(b.line, b.col + usize::from(b.width) - 1),
                });
                i += n;
            } else {
                i += 1;
            }
        }
    }

    /// Keep the current match on the same text after a rescan, or choose one.
    fn settle(&mut self, keep: Option<Point>, anchor: u64) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        let at = |matches: &[Match], p: Point| matches.partition_point(|m| m.start < p);
        let last = self.matches.len() - 1;
        let index = match keep {
            Some(p) => at(&self.matches, p).min(last),
            // The first match at or after the top of the view, else the last.
            None => at(&self.matches, Point::new(anchor, 0)).min(last),
        };
        self.current = Some(index);
    }

    /// Select the next match (wrapping) and return it.
    pub fn next(&mut self) -> Option<Match> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        self.current = Some(self.current.map_or(0, |c| (c + 1) % len));
        self.current()
    }

    /// Select the previous match (wrapping) and return it.
    pub fn prev(&mut self) -> Option<Match> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        self.current = Some(self.current.map_or(len - 1, |c| (c + len - 1) % len));
        self.current()
    }

    /// The columns to highlight on `line` as (first, last, is the current
    /// match), for matches that touch it; `cols` is the screen width.
    pub fn spans_on(&self, line: u64, cols: usize) -> Vec<(usize, usize, bool)> {
        let from = self.matches.partition_point(|m| m.end.line < line);
        let last = cols.saturating_sub(1);
        let mut out = Vec::new();
        for (i, m) in self.matches[from..].iter().enumerate() {
            if m.start.line > line {
                break;
            }
            let a = if m.start.line == line { m.start.col } else { 0 };
            let b = if m.end.line == line { m.end.col } else { last };
            out.push((a.min(last), b.min(last), self.current == Some(from + i)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(term: &Term, needle: &str, case: bool) -> Search {
        let mut s = Search::new(needle, case);
        while !s.update(term, DEFAULT_BUDGET, term.screen_top()) {}
        s
    }

    fn text_at(term: &Term, m: Match) -> String {
        let mut out = String::new();
        for line in m.start.line..=m.end.line {
            let row = term.line(line).unwrap();
            let from = if line == m.start.line { m.start.col } else { 0 };
            let to = if line == m.end.line { m.end.col } else { term.cols() - 1 };
            for col in from..=to {
                if let Some(c) = row.cells.get(col).filter(|c| c.width != 0) {
                    out.push(c.ch);
                } else if col >= row.cells.len() {
                    out.push(' ');
                }
            }
        }
        out
    }

    #[test]
    fn plain_matches_in_screen_and_history() {
        let mut t = Term::new(20, 4);
        for n in 0..30 {
            t.feed(format!("line {n} word\r\n").as_bytes());
        }
        let s = run(&t, "word", true);
        assert_eq!(s.count(), 30);
        assert!(s.done());
        assert!(s.matches().iter().all(|m| text_at(&t, *m) == "word"));
        assert_eq!(s.matches()[0].start.line, t.first_line());
    }

    #[test]
    fn case_folding_and_the_toggle() {
        let mut t = Term::new(30, 3);
        t.feed(b"Hello hello HELLO hElLo\r\n");
        assert_eq!(run(&t, "hello", false).count(), 4);
        assert_eq!(run(&t, "hello", true).count(), 1);
        assert_eq!(run(&t, "HELLO", true).count(), 1);
        assert_eq!(run(&t, "\u{c9}t\u{e9}", false).count(), 0);
        t.feed("\u{c9}T\u{c9} \u{e9}t\u{e9}\r\n".as_bytes());
        assert_eq!(run(&t, "\u{e9}t\u{e9}", false).count(), 2);
    }

    #[test]
    fn a_match_spans_wrapped_rows() {
        let mut t = Term::new(8, 5);
        t.feed(b"abcdefghijklmnop\r\n");
        let s = run(&t, "ghijk", true);
        assert_eq!(s.count(), 1);
        let m = s.matches()[0];
        assert_ne!(m.start.line, m.end.line, "across the row break");
        assert_eq!(text_at(&t, m), "ghijk");
        // Not across a real line break.
        t.feed(b"abc\r\nxdef\r\n");
        assert_eq!(run(&t, "cx", true).count(), 0);
        assert_eq!(run(&t, "c\nx", true).count(), 0);
    }

    #[test]
    fn matches_do_not_overlap_and_empty_needles_find_nothing() {
        let mut t = Term::new(20, 3);
        t.feed(b"aaaaa\r\n");
        assert_eq!(run(&t, "aa", true).count(), 2);
        assert_eq!(run(&t, "", true).count(), 0);
        let long = "x".repeat(MAX_NEEDLE + 50);
        assert_eq!(run(&t, &long, true).count(), 0);
    }

    #[test]
    fn wide_characters_match_and_cover_both_cells() {
        let mut t = Term::new(10, 3);
        t.feed("a\u{4e2d}\u{6587}b\r\n".as_bytes());
        let s = run(&t, "\u{4e2d}\u{6587}", true);
        assert_eq!(s.count(), 1);
        let m = s.matches()[0];
        assert_eq!((m.start.col, m.end.col), (1, 4));
        assert_eq!(s.spans_on(m.start.line, 10), [(1, 4, true)]);
    }

    #[test]
    fn stepping_wraps_and_the_first_pick_follows_the_view() {
        let mut t = Term::new(20, 4);
        for n in 0..10 {
            t.feed(format!("hit {n}\r\n").as_bytes());
        }
        let mut s = Search::new("hit", true);
        let anchor = t.first_line() + 4;
        while !s.update(&t, DEFAULT_BUDGET, anchor) {}
        assert_eq!(s.current().unwrap().start.line, anchor, "the first match at or after the view");
        assert_eq!(s.current_index(), Some(5));
        let here = s.current().unwrap();
        assert_ne!(s.next().unwrap(), here);
        assert_eq!(s.prev().unwrap(), here);
        for _ in 0..s.count() {
            s.next();
        }
        assert_eq!(s.current().unwrap(), here, "a full circle");
        let mut s = run(&t, "zzz", true);
        assert_eq!((s.next(), s.prev(), s.current()), (None, None, None));
    }

    #[test]
    fn the_scan_is_bounded_per_call() {
        let mut t = Term::new(20, 5);
        t.set_scrollback(20_000);
        for n in 0..12_000 {
            t.feed(format!("row {n} needle\r\n").as_bytes());
        }
        let mut s = Search::new("needle", true);
        let (mut calls, mut before) = (0, 0);
        while !s.update(&t, 1000, t.screen_top()) {
            calls += 1;
            assert!(s.count() - before <= 1000 + 5, "no more than a budget's rows per call");
            before = s.count();
            assert!(calls < 100);
        }
        assert!(calls >= 11, "{calls} calls for 12k rows");
        assert_eq!(s.count(), 12_000);
    }

    #[test]
    fn output_while_searching_keeps_the_count_right() {
        let mut t = Term::new(20, 4);
        for n in 0..10 {
            t.feed(format!("a{n} x\r\n").as_bytes());
        }
        let mut s = run(&t, "x", true);
        assert_eq!(s.count(), 10);
        t.feed(b"another x\r\nand x x\r\n");
        while !s.update(&t, DEFAULT_BUDGET, t.screen_top()) {}
        assert_eq!(s.count(), 13);
        // Scrolled lines stay matched, in order.
        assert!(s.matches().windows(2).all(|w| w[0].start < w[1].start));
    }

    #[test]
    fn eviction_drops_matches_with_their_lines_and_keeps_the_current_one() {
        let mut t = Term::new(20, 3);
        t.set_scrollback(10);
        for n in 0..30 {
            t.feed(format!("m{n} k\r\n").as_bytes());
        }
        let mut s = run(&t, "k", true);
        let kept = s.count();
        assert!(kept <= 10 + 3);
        s.next();
        let cur = s.current().unwrap();
        for n in 30..36 {
            t.feed(format!("m{n} k\r\n").as_bytes());
        }
        while !s.update(&t, DEFAULT_BUDGET, t.screen_top()) {}
        assert!(s.matches().iter().all(|m| m.start.line >= t.first_line()), "evicted lines are gone");
        assert_eq!(s.count(), kept, "six evicted, six added");
        // The current match is the same text if its line is still there.
        if cur.start.line >= t.first_line() {
            assert_eq!(s.current().unwrap().start, cur.start);
        } else {
            assert!(s.current().is_some());
        }
    }

    #[test]
    fn a_clear_or_a_reflow_starts_over() {
        let mut t = Term::new(20, 3);
        for n in 0..20 {
            t.feed(format!("q{n}\r\n").as_bytes());
        }
        let mut s = run(&t, "q", true);
        assert_eq!(s.count(), 20);
        t.feed(b"\x1b[3J");
        while !s.update(&t, DEFAULT_BUDGET, t.screen_top()) {}
        assert_eq!(s.count(), 2, "only the screen is left");
        t.feed(b"abcdefgh q\r\n");
        t.resize(5, 3);
        s.invalidate();
        while !s.update(&t, DEFAULT_BUDGET, t.screen_top()) {}
        assert_eq!(s.count(), t.dump_text().matches('q').count(), "the same text is found after a reflow");
        for m in s.matches() {
            assert_eq!(text_at(&t, *m), "q");
        }
    }

    #[test]
    fn the_match_cap_and_hostile_content() {
        let mut t = Term::new(80, 3);
        t.set_scrollback(100_000);
        let line = "e".repeat(79);
        for _ in 0..1500 {
            t.feed(format!("{line}\r\n").as_bytes());
        }
        let s = run(&t, "e", true);
        assert_eq!(s.count(), MAX_MATCHES);
        assert!(s.truncated());
        // A runaway logical line is searched only as far as it is read.
        let mut t = Term::new(80, 3);
        t.feed("z".repeat(80 * 500).as_bytes());
        let s = run(&t, "z", true);
        assert!(s.count() > 0);
    }

    #[test]
    fn spans_cover_the_rows_a_match_crosses() {
        let mut t = Term::new(8, 5);
        t.feed(b"abcdefghijklmnop\r\n");
        let s = run(&t, "fghijk", true);
        let m = s.matches()[0];
        assert_eq!(s.spans_on(m.start.line, 8), [(5, 7, true)]);
        assert_eq!(s.spans_on(m.end.line, 8), [(0, 2, true)]);
        assert!(s.spans_on(m.end.line + 1, 8).is_empty());
    }
}

#![forbid(unsafe_code)]

//! Diagnostic markers drawn by the editor: squiggles, error line tints and
//! gutter breakpoints. Issue #11 (diagnostics) feeds them through
//! [`crate::Editor::set_markers`].

/// What a [`Marker`] means, which decides how it is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// A parse or runtime error: a squiggle and a faint line tint.
    Error,
    /// A warning: a squiggle in the warning colour.
    Warning,
    /// An informational mark; currently painted as a plain squiggle.
    Info,
    /// A debugger breakpoint, drawn as a dot in the gutter.
    Breakpoint,
}

/// A spanned diagnostic on one line.
///
/// Columns are character columns within the line (what a diagnostic reports),
/// zero-based, and `end` is exclusive; the painter expands tabs when drawing. An empty
/// span (`start == end`) marks the whole line for the kinds that tint it, as a
/// breakpoint does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// The zero-based line.
    pub line: usize,
    /// The first character column of the mark.
    pub start: usize,
    /// The column just past the mark.
    pub end: usize,
    /// The kind of mark.
    pub kind: MarkerKind,
}

impl Marker {
    /// A marker spanning `start..end` character columns on `line`.
    pub fn new(line: usize, start: usize, end: usize, kind: MarkerKind) -> Marker {
        Marker {
            line,
            start,
            end,
            kind,
        }
    }

    /// A whole-line marker, used for error tints and breakpoints.
    pub fn line(line: usize, kind: MarkerKind) -> Marker {
        Marker::new(line, 0, 0, kind)
    }

    /// Whether the marker spans a range of columns.
    pub fn has_span(&self) -> bool {
        self.end > self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_marker_has_a_span() {
        let marker = Marker::new(3, 1, 5, MarkerKind::Error);
        assert!(marker.has_span());
        assert_eq!(marker.line, 3);
    }

    #[test]
    fn a_line_marker_has_no_span() {
        assert!(!Marker::line(2, MarkerKind::Error).has_span());
    }
}

//! Find-next/find-previous commands on the [`Editor`](super::Editor).

use crate::find;

use super::Editor;

impl<M: 'static> Editor<M> {
    /// Finds `query` relative to the caret, returning the matched char range.
    ///
    /// A forward search starts at the caret and wraps to the top; a backward
    /// search takes the last match before the caret and wraps to the bottom. An
    /// invalid regular expression is reported as an error message.
    pub fn find(
        &self,
        query: &find::Query,
        case_sensitive: bool,
        forward: bool,
    ) -> std::result::Result<Option<(usize, usize)>, String> {
        let state = self.state.borrow();
        let text = state.buffer.text();
        let found = find::matches(&text, query, case_sensitive)?;
        // Search from the ordered selection bounds, not the caret: the caret
        // sits at one end of the match just selected (the far end after a
        // forward find), so a backward find from it would pick that match again.
        let caret = state.view.caret;
        let (from, to) = state.view.selection().unwrap_or((caret, caret));
        let chosen = if forward {
            found
                .iter()
                .copied()
                .find(|(start, _)| *start >= to)
                .or_else(|| found.first().copied())
        } else {
            found
                .iter()
                .rev()
                .copied()
                .find(|(_, end)| *end <= from)
                .or_else(|| found.last().copied())
        };
        Ok(chosen)
    }

    /// Selects the next (or previous) match of `query`. Returns whether one was
    /// found; an invalid regular expression is reported as an error.
    pub fn find_next(
        &self,
        query: &find::Query,
        case_sensitive: bool,
        forward: bool,
    ) -> std::result::Result<bool, String> {
        match self.find(query, case_sensitive, forward)? {
            Some((start, end)) => {
                self.select(start, end);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

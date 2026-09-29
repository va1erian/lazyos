#![forbid(unsafe_code)]

//! A find/replace session on top of [`find`](crate::find), so an app's UI stays
//! thin: it owns the query text and its two options, and the functions here turn
//! them into matches, a "3 of 17" index and replacement text.
//!
//! Everything works in the *char* offsets the editor and [`find`](crate::find)
//! use, so a match on a line with multi-byte characters lands correctly. An
//! invalid regular expression is an `Err(String)` for the status area, never a
//! panic; an empty query yields no matches.

use crate::find::{self, Match, Query};

/// The find/replace session: what the user typed and which options are on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchState {
    /// The pattern text, as typed.
    pub query_text: String,
    /// Whether `query_text` is a regular expression rather than literal text.
    pub regex: bool,
    /// Whether matching is case-sensitive.
    pub case_sensitive: bool,
}

impl SearchState {
    /// The session's query.
    pub fn query(&self) -> Query {
        if self.regex {
            Query::regex(&self.query_text)
        } else {
            Query::literal(&self.query_text)
        }
    }

    /// Whether the query is empty, and so matches nothing.
    pub fn is_empty(&self) -> bool {
        self.query_text.is_empty()
    }
}

/// Every match of the session's query, as char ranges in order.
///
/// An empty query gives an empty list; an invalid regular expression gives the
/// error text for the status area.
pub fn matches(text: &str, state: &SearchState) -> Result<Vec<Match>, String> {
    if state.is_empty() {
        return Ok(Vec::new());
    }
    find::matches(text, &state.query(), state.case_sensitive)
}

/// How many times the session's query matches `text`.
pub fn match_count(text: &str, state: &SearchState) -> Result<usize, String> {
    Ok(matches(text, state)?.len())
}

/// The zero-based index of `selection` among the matches, for "3 of 17".
///
/// `None` when nothing is selected, the query is empty, the selection is not a
/// match, or the query is invalid.
pub fn current_index(text: &str, state: &SearchState, selection: Option<Match>) -> Option<usize> {
    let selection = selection?;
    matches(text, state)
        .ok()?
        .iter()
        .position(|found| *found == selection)
}

/// The whole text with every match replaced, or `None` when nothing changed.
///
/// A regular-expression replacement may use capture references such as `$1`; a
/// literal query inserts the replacement verbatim. An invalid regular expression
/// is reported as `Err`.
pub fn replace_all(
    text: &str,
    state: &SearchState,
    replacement: &str,
) -> Result<Option<String>, String> {
    if state.is_empty() {
        return Ok(None);
    }
    find::replace_all(text, &state.query(), state.case_sensitive, replacement)
}

/// The text to replace the current match with, when `selection` is itself a
/// match of the query; `None` otherwise.
///
/// A regular-expression replacement may use capture references such as `$1`.
/// This reuses the same replacement path as [`replace_all`], run over just the
/// matched text, so both expand capture groups the same way.
pub fn replacement_for(
    text: &str,
    state: &SearchState,
    selection: Option<Match>,
    replacement: &str,
) -> Result<Option<String>, String> {
    let Some((start, end)) = selection else {
        return Ok(None);
    };
    if state.is_empty() {
        return Ok(None);
    }
    if !matches(text, state)?.contains(&(start, end)) {
        return Ok(None);
    }
    let matched: String = text
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    let replaced = find::replace_all(&matched, &state.query(), state.case_sensitive, replacement)?;
    // When the replacement leaves the match unchanged there is nothing to do;
    // keeping the match is safer than expanding a literal `$` reference.
    Ok(Some(replaced.unwrap_or(matched)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(query: &str) -> SearchState {
        SearchState {
            query_text: query.to_string(),
            ..SearchState::default()
        }
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let empty = SearchState::default();
        assert!(matches("abc", &empty).expect("ok").is_empty());
        assert_eq!(match_count("abc", &empty).expect("ok"), 0);
        assert_eq!(replace_all("abc", &empty, "x").expect("ok"), None);
    }

    #[test]
    fn an_invalid_regex_is_an_error_not_a_panic() {
        let bad = SearchState {
            query_text: "(".to_string(),
            regex: true,
            case_sensitive: true,
        };
        assert!(matches("abc", &bad).is_err());
        assert!(replace_all("abc", &bad, "x").is_err());
        assert_eq!(
            replacement_for("abc", &bad, Some((0, 1)), "x"),
            Err(matches("abc", &bad).unwrap_err())
        );
    }

    #[test]
    fn a_literal_query_is_case_insensitive_when_asked() {
        let mut insensitive = state("one");
        assert_eq!(match_count("ONE one", &insensitive).expect("ok"), 2);
        insensitive.case_sensitive = true;
        assert_eq!(match_count("ONE one", &insensitive).expect("ok"), 1);
    }

    #[test]
    fn a_regex_can_replace_with_capture_groups() {
        let capture = SearchState {
            query_text: r"(\w+)_(\w+)".to_string(),
            regex: true,
            case_sensitive: true,
        };
        let replaced = replace_all("cmdHello_Click", &capture, "${2}_${1}").expect("ok");
        assert_eq!(replaced.as_deref(), Some("Click_cmdHello"));
    }

    #[test]
    fn replacement_for_current_expands_capture_groups_in_the_match() {
        let capture = SearchState {
            query_text: r"(\w+)_(\w+)".to_string(),
            regex: true,
            case_sensitive: true,
        };
        let replaced =
            replacement_for("cmdHello_Click", &capture, Some((0, 14)), "${2}_${1}").expect("ok");
        assert_eq!(replaced.as_deref(), Some("Click_cmdHello"));
    }

    #[test]
    fn replacement_for_is_none_when_the_selection_is_not_a_match() {
        let query = state("one");
        assert_eq!(
            replacement_for("one two", &query, Some((4, 7)), "x").expect("ok"),
            None
        );
        assert_eq!(
            replacement_for("one two", &query, None, "x").expect("ok"),
            None
        );
        assert_eq!(
            replacement_for("", &SearchState::default(), Some((0, 0)), "x").expect("ok"),
            None
        );
    }

    #[test]
    fn current_index_finds_the_selected_match() {
        let query = state("one");
        assert_eq!(current_index("one two one", &query, Some((0, 3))), Some(0));
        assert_eq!(current_index("one two one", &query, Some((8, 11))), Some(1));
        assert_eq!(current_index("one two one", &query, Some((4, 7))), None);
        assert_eq!(current_index("one two one", &query, None), None);
    }

    #[test]
    fn matches_use_char_offsets_for_multibyte_text() {
        let query = state("ll");
        assert_eq!(matches("héllo", &query).expect("ok"), vec![(2, 4)]);
    }

    #[test]
    fn matches_use_char_offsets_for_emoji_and_cjk() {
        // `a`, emoji, CJK, `b`, CJK: four bytes for the emoji and three per CJK
        // character, so byte offsets would land wrong.
        let text = "a😀中b中";
        assert_eq!(
            matches(text, &state("中")).expect("ok"),
            vec![(2, 3), (4, 5)]
        );
        assert_eq!(current_index(text, &state("中"), Some((4, 5))), Some(1));
        assert_eq!(match_count(text, &state("😀")).expect("ok"), 1);
    }

    #[test]
    fn matches_at_the_start_and_end_are_included() {
        let query = state("ab");
        assert_eq!(matches("abxab", &query).expect("ok"), vec![(0, 2), (3, 5)]);
    }

    #[test]
    fn non_overlapping_matches_are_returned() {
        let query = state("aa");
        assert_eq!(matches("aaaa", &query).expect("ok"), vec![(0, 2), (2, 4)]);
    }
}

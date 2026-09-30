#![forbid(unsafe_code)]

//! Find and replace, plain or by regular expression.
//!
//! The rules live here, free of xui and of the event loop, so they are
//! unit-tested directly. Everything works in *char* indices, the same unit the
//! [`Buffer`](crate::Buffer) and the caret use, so a match on a line with
//! multi-byte characters lands the caret on the right cell. The optional regex
//! dialect is that of the `regex` crate.
//!
//! A plain query is escaped and run through the same regex machinery, so there
//! is one matching path. A query wrapped in slashes (`/foo\d+/`) is treated as
//! a regular expression; everything else is matched literally. The exchange
//! format is deliberately tiny: the IDE's prompt dialog is a single text field.

use regex::{Regex, RegexBuilder};

/// A parsed find query: the pattern and whether it is a regular expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// The pattern to search for.
    pub pattern: String,
    /// Whether `pattern` is a regular expression rather than literal text.
    pub regex: bool,
}

impl Query {
    /// A literal query.
    pub fn literal(pattern: impl Into<String>) -> Query {
        Query {
            pattern: pattern.into(),
            regex: false,
        }
    }

    /// A regular-expression query.
    pub fn regex(pattern: impl Into<String>) -> Query {
        Query {
            pattern: pattern.into(),
            regex: true,
        }
    }

    /// Parses the prompt's convention: a query wrapped in `/…/` is a regular
    /// expression, anything else is literal.
    pub fn parse(text: &str) -> Query {
        if let Some(inner) = text
            .strip_prefix('/')
            .and_then(|rest| rest.strip_suffix('/'))
            && text.len() >= 2
        {
            return Query::regex(inner);
        }
        Query::literal(text)
    }
}

/// A match as a half-open char range `start..end`.
pub type Match = (usize, usize);

/// The compiled matcher for a query.
fn matcher(query: &Query, case_sensitive: bool) -> Result<Regex, String> {
    let pattern = if query.regex {
        query.pattern.clone()
    } else {
        regex::escape(&query.pattern)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| error.to_string())
}

/// Every non-overlapping match of `query`, as char ranges in order.
///
/// An empty query (or a pattern that never matches) yields no matches, and a
/// zero-length match (such as ``) is skipped: there is nothing to select, and
/// repeating a search would land on it again. An invalid regular expression is
/// reported as `Err`.
pub fn matches(text: &str, query: &Query, case_sensitive: bool) -> Result<Vec<Match>, String> {
    if query.pattern.is_empty() {
        return Ok(Vec::new());
    }
    let regex = matcher(query, case_sensitive)?;
    // One forward scan: matches arrive in order, so the char index is carried
    // from the previous byte offset instead of recounted from the start. Every
    // regex match sits on a character boundary.
    let (mut byte, mut chars) = (0usize, 0usize);
    let mut advance = |to: usize| {
        chars += text[byte..to].chars().count();
        byte = to;
        chars
    };
    Ok(regex
        .find_iter(text)
        .filter(|m| !m.is_empty())
        .map(|m| {
            let start = advance(m.start());
            (start, advance(m.end()))
        })
        .collect())
}

/// Replaces every match of `query` with `replacement`, returning the new text
/// when it differs from `text`.
///
/// For a regular-expression query the replacement may use capture references
/// such as `$1`; a literal query inserts `replacement` verbatim.
pub fn replace_all(
    text: &str,
    query: &Query,
    case_sensitive: bool,
    replacement: &str,
) -> Result<Option<String>, String> {
    if query.pattern.is_empty() {
        return Ok(None);
    }
    let regex = matcher(query, case_sensitive)?;
    let replaced = if query.regex {
        regex.replace_all(text, replacement)
    } else {
        regex.replace_all(text, regex::NoExpand(replacement))
    };
    Ok((replaced != text).then(|| replaced.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_wrapped_query_is_a_regular_expression() {
        assert_eq!(Query::parse("plain text"), Query::literal("plain text"));
        assert_eq!(Query::parse("/foo\\d+/"), Query::regex("foo\\d+"));
        // A lone slash is not a wrapper.
        assert_eq!(Query::parse("/"), Query::literal("/"));
    }

    #[test]
    fn plain_matches_land_on_char_ranges() {
        let found = matches("one two one", &Query::literal("one"), true).expect("a valid query");
        assert_eq!(found, [(0, 3), (8, 11)]);
    }

    #[test]
    fn a_plain_query_is_case_insensitive_when_asked() {
        let query = Query::literal("ONE");
        assert!(matches("one", &query, true).expect("ok").is_empty());
        assert_eq!(matches("one", &query, false).expect("ok"), [(0, 3)]);
    }

    #[test]
    fn a_bad_regular_expression_is_reported() {
        let error = matches("x", &Query::regex("("), true).expect_err("unbalanced group");
        assert!(!error.is_empty());
    }

    #[test]
    fn a_regex_can_replace_with_capture_groups() {
        let query = Query::regex(r"(\w+)_(\w+)");
        let replaced = replace_all("cmdHello_Click", &query, true, "${2}_${1}").expect("ok");
        assert_eq!(replaced.as_deref(), Some("Click_cmdHello"));
    }

    #[test]
    fn a_literal_replacement_keeps_dollar_signs() {
        let query = Query::literal("x");
        let replaced = replace_all("x", &query, true, "$1").expect("ok");
        assert_eq!(replaced.as_deref(), Some("$1"));
    }

    #[test]
    fn replacing_when_nothing_matches_is_none() {
        let query = Query::literal("zzz");
        assert_eq!(replace_all("abc", &query, true, "y").expect("ok"), None);
    }

    #[test]
    fn zero_length_matches_are_skipped() {
        let found = matches("ab cd", &Query::regex(r""), true).expect("ok");
        assert!(found.is_empty());
    }

    #[test]
    fn matches_after_a_multibyte_character_use_char_offsets() {
        let found = matches("héllo", &Query::literal("llo"), true).expect("ok");
        assert_eq!(found, [(2, 5)], "the range is in chars, not bytes");
    }
}

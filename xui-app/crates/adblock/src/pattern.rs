//! A filter's pattern as a WebKit `url-filter`.
//!
//! WebKit compiles a url-filter with its own parser (`URLFilterParser.cpp`), and a
//! single pattern it refuses fails the whole list, so this module emits only what
//! that parser accepts: ASCII literals, `.`, character classes, the quantifiers `*`,
//! `+` and `?`, groups **without** alternation, `^` first and `$` last. No `|`, no
//! `{n,m}`, no `\b`, `\d`, `\w` or `\s` outside what is translated here.

use crate::stats::DropReason;

/// Adblock Plus's `^`: any character but a letter, digit, `_`, `-`, `.` or `%`.
const SEPARATOR: &str = "[^a-zA-Z0-9_.%-]";
/// `||`: the scheme, then the host or one of its parent domains. A request URL always
/// has a path (`http://example.com/`), so a trailing `^` after a host always finds one.
const DOMAIN_START: &str = "^[^:]+://+([^:/?#]+\\.)?";

/// Generous bounds: EasyList's longest pattern is a few hundred characters and its most
/// wildcards a handful.
const MAX_PATTERN_LEN: usize = 2048;
const MAX_WILDCARDS: usize = 16;

/// The `url-filter` for `pattern`, or why there is none.
pub(crate) fn url_filter(pattern: &str) -> Result<String, DropReason> {
    if !pattern.is_ascii() {
        return Err(DropReason::NonAscii);
    }
    if pattern
        .bytes()
        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(DropReason::UnsupportedPattern);
    }
    // WebKit compiles every pattern of a list into one automaton, in the browser's own process: a
    // pattern with many wildcards (the states multiply) or a very long one is not an ad filter.
    if pattern.len() > MAX_PATTERN_LEN
        || pattern.bytes().filter(|&b| b == b'*').count() > MAX_WILDCARDS
    {
        return Err(DropReason::UnsupportedPattern);
    }
    match regex_filter_body(pattern) {
        Some(inner) => regex_subset(inner),
        None => Ok(glob(pattern)),
    }
}

/// `/.../` is a regular expression in Adblock Plus. A plain `/word/` is more often a
/// path than a regex, and reading it as a path matches less, so only a pattern with
/// regex syntax inside (`\ [ ] ( ) { } | + ? ^ $`) is taken as one.
fn regex_filter_body(pattern: &str) -> Option<&str> {
    let inner = pattern.strip_prefix('/')?.strip_suffix('/')?;
    if inner.is_empty() {
        return None;
    }
    let regexy = inner.bytes().any(|b| b"\\[](){}|+?^$".contains(&b));
    regexy.then_some(inner)
}

/// Adblock Plus syntax: `||` and `|` anchors, `*`, `^`, literals.
fn glob(pattern: &str) -> String {
    let mut rest = pattern;
    let mut out = String::new();
    if let Some(r) = rest.strip_prefix("||") {
        out.push_str(DOMAIN_START);
        rest = r;
    } else if let Some(r) = rest.strip_prefix('|') {
        out.push('^');
        rest = r;
    }
    let end_anchor = rest.ends_with('|');
    if end_anchor {
        rest = &rest[..rest.len() - 1];
    }

    let mut body = String::new();
    for b in rest.bytes() {
        match b {
            b'*' => {
                if !body.ends_with(".*") {
                    body.push_str(".*");
                }
            }
            b'^' => body.push_str(SEPARATOR),
            b'.' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'\\' | b'$' | b'|' => {
                body.push('\\');
                body.push(b as char);
            }
            _ => body.push(b as char),
        }
    }
    // WebKit unanchors a pattern by itself; a leading or trailing `.*` only costs states.
    if out.is_empty() {
        while let Some(stripped) = body.strip_prefix(".*") {
            body = stripped.to_owned();
        }
    }
    if !end_anchor {
        while let Some(stripped) = body.strip_suffix(".*") {
            // `\.*`, a literal dot repeated, is not a wildcard: keep it.
            if stripped.ends_with('\\') {
                break;
            }
            body = stripped.to_owned();
        }
    }
    out.push_str(&body);
    if end_anchor {
        out.push('$');
    }
    if out.is_empty() {
        out.push_str(".*");
    }
    out
}

/// Checks a regular expression against WebKit's subset and translates `\d` and `\w`.
fn regex_subset(inner: &str) -> Result<String, DropReason> {
    let unsupported = Err(DropReason::UnsupportedPattern);
    let b = inner.as_bytes();
    let mut out = String::new();
    let mut depth = 0usize;
    // Whether the previous token is something a quantifier may follow, and whether it
    // already has one (`a*?` is a lazy quantifier, which WebKit does not take).
    let (mut atom, mut quantified) = (false, false);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' => {
                i += 1;
                let Some(&n) = b.get(i) else {
                    return unsupported;
                };
                match n {
                    b'd' => out.push_str("[0-9]"),
                    b'w' => out.push_str("[a-zA-Z0-9_]"),
                    n if n.is_ascii_alphanumeric() => return unsupported,
                    n => {
                        out.push('\\');
                        out.push(n as char);
                    }
                }
                (atom, quantified) = (true, false);
            }
            b'[' => {
                out.push('[');
                i += 1;
                if b.get(i) == Some(&b'^') {
                    out.push('^');
                    i += 1;
                }
                let start = i;
                // The members as written, to check the ranges once the class is closed.
                let mut members: Vec<u8> = Vec::new();
                loop {
                    let Some(&k) = b.get(i) else {
                        return unsupported;
                    };
                    match k {
                        b']' if i > start => {
                            out.push(']');
                            break;
                        }
                        b']' | b'[' => return unsupported,
                        b'\\' => {
                            i += 1;
                            match b.get(i) {
                                // `\-` is a literal dash to one parser and a range to
                                // another: not worth the ambiguity.
                                Some(&n) if !n.is_ascii_alphanumeric() && n != b'-' => {
                                    out.push('\\');
                                    out.push(n as char);
                                    members.push(n);
                                }
                                _ => return unsupported,
                            }
                        }
                        _ => {
                            out.push(k as char);
                            members.push(k);
                        }
                    }
                    i += 1;
                }
                // `a-z` is a range; a `-` first or last is itself; `z-a` is an error.
                let mut m = 0;
                while m < members.len() {
                    if m + 2 < members.len() && members[m + 1] == b'-' {
                        if members[m] > members[m + 2] {
                            return unsupported;
                        }
                        m += 3;
                    } else {
                        m += 1;
                    }
                }
                (atom, quantified) = (true, false);
            }
            b'(' => {
                if b.get(i + 1) == Some(&b'?') || b.get(i + 1) == Some(&b')') {
                    return unsupported;
                }
                depth += 1;
                out.push('(');
                (atom, quantified) = (false, false);
            }
            b')' => {
                if depth == 0 {
                    return unsupported;
                }
                depth -= 1;
                out.push(')');
                (atom, quantified) = (true, false);
            }
            b'*' | b'+' | b'?' => {
                if !atom || quantified {
                    return unsupported;
                }
                out.push(c as char);
                quantified = true;
            }
            b'|' | b'{' | b'}' | b']' => return unsupported,
            b'^' => {
                if i != 0 {
                    return unsupported;
                }
                out.push('^');
                (atom, quantified) = (false, false);
            }
            b'$' => {
                if i != b.len() - 1 {
                    return unsupported;
                }
                out.push('$');
                (atom, quantified) = (false, false);
            }
            _ => {
                out.push(c as char);
                (atom, quantified) = (true, false);
            }
        }
        i += 1;
    }
    // A regular expression that matches the empty string matches every address: a rule that
    // blocks everything the page asks for, whatever the list meant.
    if depth != 0 || out.is_empty() || matches_empty(&out) {
        return unsupported;
    }
    Ok(out)
}

/// Whether `regex` (already inside the subset: no alternation, no counted repeats) matches the
/// empty string: every element of the sequence is optional or an anchor.
fn matches_empty(regex: &str) -> bool {
    let b = regex.as_bytes();
    // One entry per open group: whether everything in it so far can be empty.
    let mut open = vec![true];
    let mut i = 0;
    while i < b.len() {
        // The element at `i`: whether it can be empty before a quantifier is looked at.
        let mut empty = match b[i] {
            b'^' | b'$' => true,
            b'(' => {
                open.push(true);
                i += 1;
                continue;
            }
            b')' => open.pop().unwrap_or(true),
            b'\\' => {
                i += 1;
                false
            }
            b'[' => {
                // To the closing bracket; an escaped bracket is not one.
                while i + 1 < b.len() && b[i + 1] != b']' {
                    i += usize::from(b[i + 1] == b'\\') + 1;
                }
                i += 1;
                false
            }
            _ => false,
        };
        i += 1;
        if matches!(b.get(i), Some(b'*' | b'?')) {
            empty = true;
            i += 1;
        } else if b.get(i) == Some(&b'+') {
            i += 1;
        }
        if let Some(top) = open.last_mut() {
            *top &= empty;
        }
    }
    open.first().copied().unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(p: &str) -> String {
        url_filter(p).unwrap()
    }

    #[test]
    fn domain_anchor_and_separator() {
        assert_eq!(
            f("||example.com^"),
            r"^[^:]+://+([^:/?#]+\.)?example\.com[^a-zA-Z0-9_.%-]"
        );
        assert_eq!(
            f("||ads.example.com/banner"),
            r"^[^:]+://+([^:/?#]+\.)?ads\.example\.com/banner"
        );
    }

    #[test]
    fn start_and_end_anchors() {
        assert_eq!(f("|https://x.com/"), r"^https://x\.com/");
        assert_eq!(f("swf|"), r"swf$");
        assert_eq!(f("|http://a|"), r"^http://a$");
    }

    #[test]
    fn wildcards_collapse_and_edges_are_trimmed() {
        assert_eq!(f("/ads/*"), "/ads/");
        assert_eq!(f("*banner*"), "banner");
        assert_eq!(f("/a**b/"), "/a.*b/");
        assert_eq!(f("*"), ".*");
        assert_eq!(f(""), ".*");
        // A wildcard before an end anchor is kept.
        assert_eq!(f("ad*|"), r"ad.*$");
    }

    #[test]
    fn regex_metacharacters_in_a_glob_are_literals() {
        assert_eq!(f("a+b?(c)"), r"a\+b\?\(c\)");
        assert_eq!(f("x$y"), r"x\$y");
    }

    #[test]
    fn a_slash_pair_is_a_path_unless_it_is_regex_syntax() {
        assert_eq!(f("/ads/"), "/ads/");
        assert_eq!(f("/banner.*ad/"), r"/banner\..*ad/");
        assert_eq!(f(r"/ad[0-9]+\.js/"), r"ad[0-9]+\.js");
        assert_eq!(f(r"/^https?:\/\/x\.com\//"), r"^https?:\/\/x\.com\/");
        assert_eq!(f(r"/\d+\.gif$/"), r"[0-9]+\.gif$");
    }

    #[test]
    fn regex_outside_the_subset_is_refused() {
        for p in [
            "/(ad|banner)/",
            r"/ad\b/",
            "/ad{2,3}/",
            r"/\s+/",
            "/ad+?/",
            "/a(?:b)/",
            "/a^b+/",
            "/a$b+/",
            "/()+/",
            "/[]a]+/",
            "/(a+/",
            // WebKit refuses a reversed range, and one bad pattern fails the whole list.
            "/[z-a]+/",
            "/[/$match-case$script]?/",
            r"/[a\-z]+/",
        ] {
            assert_eq!(url_filter(p), Err(DropReason::UnsupportedPattern), "{p}");
        }
    }

    #[test]
    fn a_regex_that_matches_the_empty_string_is_refused() {
        for r in [
            "/^/",
            "/(a)?/",
            "/a?b*/",
            "/(a*)(b?)/",
            "/(a*)+/",
            "/^(a)?$/",
            "/[a-z]*/",
        ] {
            assert!(url_filter(r).is_err(), "{r}");
        }
        for r in [
            "/^a/",
            "/a+/",
            "/(a?)b/",
            "/ads?[0-9]/",
            "/\\.js$/",
            "/(a)+/",
            "/[\\]]+/",
        ] {
            assert!(url_filter(r).is_ok(), "{r}");
        }
    }

    #[test]
    fn non_ascii_and_whitespace_are_refused() {
        assert_eq!(url_filter("||bücher.de^"), Err(DropReason::NonAscii));
        assert_eq!(url_filter("a b"), Err(DropReason::UnsupportedPattern));
    }
}

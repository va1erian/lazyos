//! URL detection in terminal text: a small, conservative matcher (no regex
//! engine) for `http://`, `https://`, `mailto:` and `file://`.
//!
//! The rules favour not linking over linking something wrong: the scheme must
//! start a word, the part after it must look like a target, the URL ends at
//! whitespace, a quote, an angle bracket or a control character, trailing
//! punctuation is not part of it (and a closing bracket only when its opener is
//! inside), and nothing longer than [`MAX_URL`] is a link.

/// The longest URL recognised, in characters.
pub const MAX_URL: usize = 2048;

const SCHEMES: [&str; 4] = ["https://", "http://", "mailto:", "file://"];

fn is_stop(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\' | '^' | '{' | '}' | '|')
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `chars[at..]` starts with `scheme` (ASCII case-insensitively).
fn starts_with(chars: &[char], at: usize, scheme: &str) -> bool {
    let wanted = scheme.chars();
    chars.len() >= at + scheme.len()
        && chars[at..].iter().zip(wanted).all(|(c, w)| c.to_ascii_lowercase() == w)
}

/// The end (exclusive) of the URL that starts at `start` with a scheme of
/// `scheme_len` characters, or `None` when what follows is not a target.
fn end_of(chars: &[char], start: usize, scheme: &str) -> Option<usize> {
    let body = start + scheme.len();
    let mut end = body;
    while end < chars.len() && !is_stop(chars[end]) && end - start < MAX_URL + 1 {
        end += 1;
    }
    if end - start > MAX_URL {
        return None;
    }
    // Trailing punctuation belongs to the sentence; a closer belongs to the URL
    // only if its opener is inside it.
    while end > body {
        let last = chars[end - 1];
        let opener = match last {
            ')' => Some('('),
            ']' => Some('['),
            _ => None,
        };
        let balanced = opener.is_some_and(|o| chars[start..end].iter().filter(|&&c| c == o).count()
            >= chars[start..end].iter().filter(|&&c| c == last).count());
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '*' | '~') || (opener.is_some() && !balanced) {
            end -= 1;
        } else {
            break;
        }
    }
    let target = &chars[body..end];
    let ok = match scheme {
        "mailto:" => {
            // user@host, both sides non-empty, the host with a letter or digit.
            let at = target.iter().position(|&c| c == '@')?;
            at > 0 && target[at + 1..].iter().any(|c| c.is_alphanumeric())
        }
        "file://" => target.first() == Some(&'/') && target.len() > 1,
        _ => {
            // http(s): a host that starts with a letter or digit.
            target.first().is_some_and(|c| c.is_alphanumeric()) && target.iter().any(|c| c.is_alphanumeric())
        }
    };
    ok.then_some(end)
}

/// The URLs in `chars` as (first, one past last) character indexes, in order
/// and not overlapping.
pub fn find_urls(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let boundary = at == 0 || !word_char(chars[at - 1]);
        let scheme = boundary
            .then(|| SCHEMES.iter().find(|s| starts_with(chars, at, s)))
            .flatten();
        match scheme.and_then(|s| end_of(chars, at, s).map(|end| (end, s))) {
            Some((end, _)) => {
                out.push((at, end));
                at = end;
            }
            None => at += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars).into_iter().map(|(a, b)| chars[a..b].iter().collect()).collect()
    }

    #[test]
    fn the_four_schemes() {
        assert_eq!(urls("see http://example.com/ now"), ["http://example.com/"]);
        assert_eq!(urls("https://a.b/c?d=e&f=g#h"), ["https://a.b/c?d=e&f=g#h"]);
        assert_eq!(urls("mail mailto:me@example.org please"), ["mailto:me@example.org"]);
        assert_eq!(urls("open file:///tmp/a%20b.txt"), ["file:///tmp/a%20b.txt"]);
        assert_eq!(urls("HTTP://UPPER.example"), ["HTTP://UPPER.example"]);
    }

    #[test]
    fn punctuation_ends_it_and_brackets_balance() {
        assert_eq!(urls("go to http://example.com."), ["http://example.com"]);
        assert_eq!(urls("(see http://example.com/a)"), ["http://example.com/a"]);
        assert_eq!(urls("http://example.com/a_(b)"), ["http://example.com/a_(b)"]);
        assert_eq!(urls("[http://example.com/x]"), ["http://example.com/x"]);
        assert_eq!(urls("\"http://example.com/\", and <http://example.org/x>"), ["http://example.com/", "http://example.org/x"]);
        assert_eq!(urls("http://example.com/?!;"), ["http://example.com/"]);
    }

    #[test]
    fn several_and_adjacent() {
        assert_eq!(urls("http://a.b http://c.d"), ["http://a.b", "http://c.d"]);
        assert_eq!(urls("http://a.bhttp://c.d"), ["http://a.bhttp://c.d"], "one URL, the scheme is mid-word");
    }

    #[test]
    fn what_is_not_a_link() {
        for text in [
            "http://",
            "http:// x",
            "http://.",
            "xhttp://example.com",
            "ahttps://example.com",
            "mailto:",
            "mailto:@x",
            "mailto:me@",
            "file://",
            "file://x",
            "ftp://example.com",
            "javascript:alert(1)",
            "data:text/html,x",
            "http//example.com",
            "",
        ] {
            assert!(urls(text).is_empty(), "{text:?} -> {:?}", urls(text));
        }
    }

    #[test]
    fn hostile_and_long_lines_stay_bounded() {
        let long = format!("http://{}", "a".repeat(5000));
        assert!(urls(&long).is_empty(), "too long to be a link");
        let ok = format!("http://{}", "a".repeat(MAX_URL - 7));
        assert_eq!(urls(&ok).len(), 1);
        let many = "http://a.b ".repeat(5000);
        assert_eq!(urls(&many).len(), 5000);
        let scheme_soup = "http://".repeat(10_000);
        let _ = urls(&scheme_soup);
        let mut nasty = String::from("http://x");
        nasty.push_str(&"(".repeat(10_000));
        let _ = urls(&nasty);
        assert!(urls("http://a.b/\u{7}x").iter().all(|u| !u.contains('\u{7}')));
    }
}

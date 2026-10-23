//! Text from outside the window (a page's title, a file somebody edited, a person's typing) made
//! fit to keep, show in a menu or put in a report: no control characters, and none of the
//! characters that reorder or hide what is shown around them (bidirectional overrides and
//! isolates, the zero-width marks), so a title cannot make a line of a report or a menu row read
//! differently from what it holds.

/// Whether `c` must never be kept: a control character, a line or paragraph separator, a
/// bidirectional embedding, override or isolate, a directional or Arabic letter mark, a zero-width
/// space or joiner mark, or the byte order mark.
pub fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// `text` without the unsafe characters. Newlines and tabs go too: use [`strip_multiline`] for text
/// that may span lines.
pub fn strip(text: &str) -> String {
    text.chars().filter(|c| !is_unsafe(*c)).collect()
}

/// `text` without the unsafe characters, keeping `\n` (a `\r\n` or a lone `\r` becomes `\n`) and
/// turning a tab into a space.
pub fn strip_multiline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            c if is_unsafe(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// The first `max` characters of `text`.
pub fn clip_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The longest prefix of `text` of at most `max` bytes that ends on a character boundary.
pub fn clip_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One line of text: unsafe characters removed, edges trimmed, at most `max` characters.
pub fn line(text: &str, max: usize) -> String {
    clip_chars(strip(text).trim(), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_reordering_marks_are_removed() {
        assert_eq!(strip("a\u{202E}b\u{0}c\nd\te\u{200B}f\u{2066}g"), "abcdefg");
        assert_eq!(strip("plain text é 日本"), "plain text é 日本");
    }

    #[test]
    fn multiline_text_keeps_its_lines_in_one_form() {
        assert_eq!(strip_multiline("a\r\nb\rc\nd\te\u{202E}f"), "a\nb\nc\nd ef");
    }

    #[test]
    fn clipping_never_splits_a_character() {
        assert_eq!(clip_chars("aé日本", 3), "aé日");
        assert_eq!(clip_bytes("aé日本", 2), "a");
        assert_eq!(clip_bytes("aé日本", 4), "aé");
        assert_eq!(clip_bytes("abc", 10), "abc");
        assert_eq!(clip_bytes("日", 0), "");
    }

    #[test]
    fn a_line_is_trimmed_and_cut() {
        assert_eq!(line("  \u{202E} hello\n world  ", 8), "hello wo");
    }
}

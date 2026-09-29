#![forbid(unsafe_code)]

//! Pure text helpers for the monospace grid: tab expansion, character-column
//! mapping and word boundaries.
//!
//! These functions never touch a backend, so they are unit-tested directly. The
//! grid is measured in *display columns*: a tab advances to the next multiple
//! of the tab width, every other character advances by one.

/// Whether `character` belongs to a word (an identifier or a number).
pub fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The class a double-click's selection run expands within: words, whitespace
/// and punctuation each select on their own.
fn char_class(character: char) -> u8 {
    if is_word_char(character) {
        0
    } else if character.is_whitespace() {
        1
    } else {
        2
    }
}

/// The display column after `character` was placed at column `col`.
pub(crate) fn advance(col: usize, character: char, tab_width: usize) -> usize {
    if character == '\t' {
        let width = tab_width.max(1);
        col + width - (col % width)
    } else {
        col + 1
    }
}

/// The display column of char position `char_col` within `line`.
pub fn display_col(line: &str, char_col: usize, tab_width: usize) -> usize {
    line.chars()
        .take(char_col)
        .fold(0, |col, character| advance(col, character, tab_width))
}

/// The char position nearest display column `display_col` within `line`.
///
/// A point past the end of the line clamps to the line's length.
pub fn char_col_for_display(line: &str, display_col: usize, tab_width: usize) -> usize {
    let mut col = 0;
    for (index, character) in line.chars().enumerate() {
        let next = advance(col, character, tab_width);
        if display_col < next {
            // Inside this character's cell: snap to the nearer boundary.
            return if display_col - col < next - display_col {
                index
            } else {
                index + 1
            };
        }
        col = next;
    }
    line.chars().count()
}

/// `line` with every tab expanded to spaces, so a monospace grid can draw it
/// directly.
pub fn expand_tabs(line: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for character in line.chars() {
        if character == '\t' {
            let next = advance(col, character, tab_width);
            for _ in col..next {
                out.push(' ');
            }
            col = next;
        } else {
            out.push(character);
            col += 1;
        }
    }
    out
}

/// The char range of the word around `char_col` within `line`. Whitespace and
/// punctuation form their own runs, so a double-click on either selects that
/// run.
pub fn word_range(line: &str, char_col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let at = char_col.min(chars.len().saturating_sub(1));
    let wanted = char_class(chars[at]);
    let mut start = at;
    while start > 0 && char_class(chars[start - 1]) == wanted {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && char_class(chars[end]) == wanted {
        end += 1;
    }
    (start, end)
}

/// The leading whitespace of `line`.
pub fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, character)| !character.is_whitespace() || *character == '\n')
        .map_or(line.len(), |(index, _)| index);
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_columns_count_chars_except_tabs() {
        assert_eq!(display_col("abc", 2, 4), 2);
        assert_eq!(display_col("\tab", 1, 4), 4);
        assert_eq!(display_col("\tab", 2, 4), 5);
        assert_eq!(display_col("a\tb", 2, 4), 4);
    }

    #[test]
    fn display_columns_round_trip() {
        for line in ["hello", "a\tb", "\t\tx", ""] {
            for col in 0..=line.chars().count() {
                let display = display_col(line, col, 4);
                assert_eq!(char_col_for_display(line, display, 4), col);
            }
        }
    }

    #[test]
    fn a_click_inside_a_cell_snaps_to_the_nearer_edge() {
        assert_eq!(char_col_for_display("abc", 0, 4), 0);
        assert_eq!(char_col_for_display("abc", 2, 4), 2);
        assert_eq!(char_col_for_display("abc", 99, 4), 3);
    }

    #[test]
    fn tabs_expand_to_the_next_stop() {
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("a\tx", 4), "a   x");
        assert_eq!(expand_tabs("abcd\tx", 4), "abcd    x");
        assert_eq!(display_col("abcd\tx", 5, 4), 8);
        assert_eq!(expand_tabs("abcd\t", 4).len(), 8);
    }

    #[test]
    fn word_range_selects_the_whole_identifier() {
        assert_eq!(word_range("let total = 1;", 5), (4, 9));
        assert_eq!(word_range("let total = 1;", 4), (4, 9));
        assert_eq!(word_range("foo_bar", 3), (0, 7));
    }

    #[test]
    fn word_range_selects_a_non_word_run() {
        assert_eq!(word_range("a + b", 1), (1, 2));
        assert_eq!(word_range("a + b", 2), (2, 3));
        assert_eq!(word_range("a   b", 2), (1, 4));
    }

    #[test]
    fn word_range_handles_an_empty_line() {
        assert_eq!(word_range("", 0), (0, 0));
    }

    #[test]
    fn leading_whitespace_is_the_indent() {
        assert_eq!(leading_whitespace("    x"), "    ");
        assert_eq!(leading_whitespace("\t\tx"), "\t\t");
        assert_eq!(leading_whitespace("x"), "");
        assert_eq!(leading_whitespace("   "), "   ");
    }
}

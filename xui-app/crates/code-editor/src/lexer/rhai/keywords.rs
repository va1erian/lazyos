//! The Rhai language's lexical predicates: keywords, identifier and symbol
//! character classes, and the punctuation set the scanner splits on.

use crate::lexer::token::is_bracket;

/// Whether `text` is a Rhai keyword or reserved word.
pub(super) fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "true"
            | "false"
            | "let"
            | "const"
            | "if"
            | "else"
            | "switch"
            | "do"
            | "while"
            | "until"
            | "loop"
            | "for"
            | "in"
            | "fn"
            | "private"
            | "continue"
            | "break"
            | "return"
            | "throw"
            | "try"
            | "catch"
            | "import"
            | "export"
            | "as"
            | "public"
            | "package"
            | "super"
            | "async"
            | "await"
            | "use"
            | "case"
            | "this"
            | "global"
            | "static"
            | "var"
    )
}

/// Whether `c` may start an identifier.
pub(super) fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// Whether `c` may continue an identifier.
pub(super) fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Whether `c` may appear in a symbolic operator.
pub(super) fn is_symbol(c: char) -> bool {
    matches!(
        c,
        '!' | '$'
            | '%'
            | '&'
            | '*'
            | '+'
            | '-'
            | '.'
            | '/'
            | ':'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '^'
            | '|'
            | '~'
            | '#'
    )
}

/// Whether `c` is punctuation rather than an operator.
pub(super) fn is_punctuation(c: char) -> bool {
    is_bracket(c) || c == ',' || c == ';'
}

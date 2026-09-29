//! Tokenizer: source text to a flat token stream.
//!
//! Newlines are emitted as `Semi` so the parser can separate statements.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    // punctuation / operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Colon,
    Assign,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
}

/// Split `source` into tokens, or return an error message.
pub fn lex(source: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' => i += 1,
            '\n' => {
                tokens.push(Tok::Semi);
                i += 1;
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let value = text
                    .parse::<f64>()
                    .map_err(|_| format!("bad number: {text}"))?;
                tokens.push(Tok::Num(value));
            }
            '"' => {
                i += 1;
                let mut text = String::new();
                while i < chars.len() && chars[i] != '"' {
                    text.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("unterminated string".to_string());
                }
                i += 1;
                tokens.push(Tok::Str(text));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            _ => {
                let (token, consumed) = lex_symbol(&chars[i..])?;
                tokens.push(token);
                i += consumed;
            }
        }
    }
    Ok(tokens)
}

/// Lex a single punctuation/operator symbol, returning it and its length.
fn lex_symbol(chars: &[char]) -> Result<(Tok, usize), String> {
    let two = |a: char, b: char| chars.len() >= 2 && chars[0] == a && chars[1] == b;
    if two('=', '=') {
        return Ok((Tok::EqEq, 2));
    }
    if two('!', '=') {
        return Ok((Tok::Ne, 2));
    }
    if two('<', '=') {
        return Ok((Tok::Le, 2));
    }
    if two('>', '=') {
        return Ok((Tok::Ge, 2));
    }
    if two('&', '&') {
        return Ok((Tok::AndAnd, 2));
    }
    if two('|', '|') {
        return Ok((Tok::OrOr, 2));
    }
    let token = match chars[0] {
        '+' => Tok::Plus,
        '-' => Tok::Minus,
        '*' => Tok::Star,
        '/' => Tok::Slash,
        '%' => Tok::Percent,
        '(' => Tok::LParen,
        ')' => Tok::RParen,
        '[' => Tok::LBracket,
        ']' => Tok::RBracket,
        '{' => Tok::LBrace,
        '}' => Tok::RBrace,
        ',' => Tok::Comma,
        ';' => Tok::Semi,
        ':' => Tok::Colon,
        '=' => Tok::Assign,
        '<' => Tok::Lt,
        '>' => Tok::Gt,
        '!' => Tok::Bang,
        other => return Err(format!("unexpected character: {other}")),
    };
    Ok((token, 1))
}

impl Tok {
    /// The literal text used in error messages.
    pub fn describe(&self) -> String {
        match self {
            Tok::Num(n) => alloc::format!("number {n}"),
            Tok::Str(_) => "string".to_string(),
            Tok::Ident(name) => alloc::format!("name '{name}'"),
            other => alloc::format!("'{other:?}'"),
        }
    }
}

//! A small Dyon-inspired language: tokenizer, parser, runtime values, a
//! tree-walking interpreter, and the line-oriented shell ([`repl::Shell`])
//! built on them.
//!
//! The crate is `no_std` + `alloc` and does no I/O of its own: output goes to
//! a caller-supplied sink and files come from a caller-supplied reader. That
//! lets the native `sh` (ring 3, `int 0x80` console) and the windowed
//! Terminal app (a `xuid` client) run the very same interpreter.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod interp;
pub mod lexer;
pub mod parser;
pub mod repl;
pub mod value;

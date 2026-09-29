//! The interactive shell layer: one input line in, output text out.
//!
//! Shared by `SH.ELF` (console) and the Terminal xui app, so both accept the
//! same commands: `help`, `quit`/`exit`, `cat <file>` and the language itself.

use alloc::string::String;
use alloc::vec::Vec;

use crate::interp::Interp;
use crate::{lexer, parser};

/// The greeting printed when a shell starts and by `help`.
pub const BANNER: &str = "LazyOS interpreter (ring 3)\n\
    Try: [1,2,3]   let x = 6*7   x*2   \"hi\" + \" there\"   cat HELLO.TXT\n";

/// The most bytes `cat` shows of one file.
pub const CAT_LIMIT: usize = 1024;

/// What the caller should do after a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Prompt for the next line.
    Continue,
    /// The user asked to leave (`quit`/`exit`).
    Exit,
}

/// A shell session: the interpreter's variables persist across lines.
#[derive(Default)]
pub struct Shell {
    interp: Interp,
}

impl Shell {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run one input line. Everything the line prints, errors included, goes
    /// through `out`; `read_file` resolves `cat` (returning at most the bytes
    /// it wants shown, or `None` for a missing file).
    pub fn exec_line(
        &mut self,
        line: &str,
        read_file: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
        out: &mut dyn FnMut(&str),
    ) -> Flow {
        let text = line.trim();
        match text {
            "" => {}
            "quit" | "exit" => return Flow::Exit,
            "help" => out(BANNER),
            _ if text.starts_with("cat ") => cat(text[4..].trim(), read_file, out),
            _ => match lexer::lex(text).and_then(parser::parse) {
                Ok(stmts) => {
                    if let Err(message) = self.interp.run(&stmts, out) {
                        report(&message, out);
                    }
                }
                Err(message) => report(&message, out),
            },
        }
        Flow::Continue
    }
}

/// `cat <file>`: print up to [`CAT_LIMIT`] bytes of a file.
fn cat(name: &str, read_file: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut dyn FnMut(&str)) {
    if name.is_empty() {
        return report("usage: cat <file>", out);
    }
    match read_file(name) {
        Some(bytes) => {
            let shown = &bytes[..bytes.len().min(CAT_LIMIT)];
            out(&String::from_utf8_lossy(shown));
        }
        None => report("file not found", out),
    }
}

/// Print an `error: ...` line.
fn report(message: &str, out: &mut dyn FnMut(&str)) {
    out("error: ");
    out(message);
    out("\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(shell: &mut Shell, line: &str) -> (Flow, String) {
        let mut text = String::new();
        let flow = shell.exec_line(line, &mut |_| None, &mut |chunk| text.push_str(chunk));
        (flow, text)
    }

    #[test]
    fn expressions_print_and_variables_persist() {
        let mut shell = Shell::new();
        assert_eq!(run(&mut shell, "let x = 6*7").1, "");
        assert_eq!(run(&mut shell, "x*2").1, "84\n");
        assert_eq!(run(&mut shell, "\"hi\" + \" there\"").1, "hi there\n");
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let mut shell = Shell::new();
        let (flow, text) = run(&mut shell, "nope");
        assert_eq!(flow, Flow::Continue);
        assert!(text.starts_with("error: "), "{text}");
    }

    #[test]
    fn quit_and_exit_leave() {
        let mut shell = Shell::new();
        assert_eq!(run(&mut shell, "quit").0, Flow::Exit);
        assert_eq!(run(&mut shell, " exit ").0, Flow::Exit);
        assert_eq!(run(&mut shell, "").0, Flow::Continue);
    }

    #[test]
    fn cat_reads_through_the_callback() {
        let mut shell = Shell::new();
        let mut text = String::new();
        shell.exec_line(
            "cat HELLO.TXT",
            &mut |name| (name == "HELLO.TXT").then(|| b"hello\n".to_vec()),
            &mut |chunk| text.push_str(chunk),
        );
        assert_eq!(text, "hello\n");
        assert!(run(&mut shell, "cat MISSING").1.contains("file not found"));
        assert!(run(&mut shell, "cat ").1.contains("error"));
    }
}

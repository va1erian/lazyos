//! The loops around `rhai_lazy::repl::Repl`: where lines come from, and where
//! results and errors go. Generic over the streams so they are tested without
//! a terminal.
//!
//! Two front ends share one session core:
//! * **interactive** (`rhai`): prompts, and the [`Editor`] echoes and edits the
//!   line, because LazyOS terminals are raw (no echo, no line discipline);
//! * **batch** (`rhai -q`): no prompts and no echo, lines read as text, for
//!   `... | rhai -q` and the scripted evidence runs.

use std::io::{self, BufRead, Read, Write};

use rhai_lazy::repl::{Repl, Step};
use rhai_lazy::Engine;

use crate::editor::{Editor, Event};

/// The longest single batch input line accepted; a longer one is discarded whole.
pub const MAX_LINE: usize = crate::editor::MAX_LINE;

/// Result of [`read_line_bounded`].
#[derive(Debug, PartialEq, Eq)]
pub enum LineRead {
    Eof,
    Line,
    /// The line exceeded the cap; it was consumed up to its newline.
    TooLong,
}

/// Read one line into `buf` (cleared first, newline kept), never buffering
/// more than `max` bytes of it.
pub fn read_line_bounded<R: BufRead>(
    input: &mut R,
    max: usize,
    buf: &mut Vec<u8>,
) -> io::Result<LineRead> {
    buf.clear();
    let read = (&mut *input).take(max as u64).read_until(b'\n', buf)?;
    if read == 0 {
        return Ok(LineRead::Eof);
    }
    if buf.last() == Some(&b'\n') || read < max {
        return Ok(LineRead::Line);
    }
    // Cap reached mid-line: throw the rest of the line away in bounded steps.
    loop {
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            break;
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(end) => {
                input.consume(end + 1);
                break;
            }
            None => {
                let all = chunk.len();
                input.consume(all);
            }
        }
    }
    Ok(LineRead::TooLong)
}

/// One unit of interactive input.
#[derive(Debug, PartialEq, Eq)]
pub enum Edited {
    Line(String),
    Interrupt,
    Eof,
}

/// Read one edited line: bytes go through the [`Editor`] as they arrive (a
/// raw terminal delivers a key at a time) and the echo is written and flushed
/// before blocking for more.
pub fn read_edited<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    editor: &mut Editor,
) -> io::Result<Edited> {
    loop {
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            // A signal interrupted the wait for a key: just wait again.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(Edited::Eof);
        }
        let mut echo = Vec::new();
        let mut used = 0;
        let mut done = None;
        for &byte in chunk {
            used += 1;
            match editor.feed(byte, &mut echo) {
                Event::Pending => {}
                Event::Line(line) => done = Some(Edited::Line(line)),
                Event::Interrupt => done = Some(Edited::Interrupt),
                Event::Eof => done = Some(Edited::Eof),
            }
            if done.is_some() {
                break;
            }
        }
        input.consume(used);
        out.write_all(&echo)?;
        out.flush()?;
        if let Some(result) = done {
            return Ok(result);
        }
    }
}

/// Short on purpose: the console is about 50 columns wide and does not wrap.
const BANNER: &str = "rhai REPL: :help, :quit or Ctrl-D";

/// How the session talks to its terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Prompts, banner, and the built-in line editor.
    Interactive,
    /// Plain text lines in, results out.
    Batch,
}

/// Run the REPL until EOF, `:quit` or `exit(n)`; returns the exit status.
pub fn run<R: BufRead, W: Write, E: Write>(
    engine: &Engine,
    style: Style,
    mut input: R,
    mut out: W,
    mut err: E,
) -> i32 {
    let interactive = style == Style::Interactive;
    let mut repl = Repl::new(engine);
    let mut editor = Editor::new();
    let mut buf = Vec::new();
    // A closed stdout ends the session quietly, like any filter.
    macro_rules! emit {
        ($stream:expr, $($arg:tt)*) => {
            if writeln!($stream, $($arg)*).and_then(|()| $stream.flush()).is_err() {
                return 0;
            }
        };
    }
    if interactive {
        emit!(out, "{BANNER}");
    }
    loop {
        if interactive
            && write!(out, "{}", repl.prompt())
                .and_then(|()| out.flush())
                .is_err()
        {
            return 0;
        }
        let line = if interactive {
            match read_edited(&mut input, &mut out, &mut editor) {
                Ok(Edited::Line(line)) => line,
                Ok(Edited::Interrupt) => {
                    repl.interrupt();
                    continue;
                }
                Ok(Edited::Eof) => {
                    emit!(out, "");
                    return 0;
                }
                Err(e) => {
                    emit!(err, "rhai: reading input: {e}");
                    return 1;
                }
            }
        } else {
            match read_line_bounded(&mut input, MAX_LINE, &mut buf) {
                Ok(LineRead::Eof) => return 0,
                Ok(LineRead::TooLong) => {
                    emit!(err, "error: line longer than {MAX_LINE} bytes; discarded");
                    continue;
                }
                Ok(LineRead::Line) => {}
                Err(e) => {
                    emit!(err, "rhai: reading input: {e}");
                    return 1;
                }
            }
            let Ok(text) = std::str::from_utf8(&buf) else {
                emit!(err, "error: input is not valid UTF-8; line ignored");
                continue;
            };
            let text = text.strip_suffix('\n').unwrap_or(text);
            text.strip_suffix('\r').unwrap_or(text).to_string()
        };
        match repl.feed(&line) {
            Step::More | Step::Quiet => {}
            Step::Show(text) => emit!(out, "{text}"),
            Step::Error(text) => emit!(err, "{text}"),
            Step::Exit(code) => return code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::rc::Rc;

    use crate::sys::StdHost;
    use rhai_lazy::{build_engine, Config};

    fn session(input: &[u8], style: Style) -> (i32, String, String) {
        let engine = build_engine(Rc::new(StdHost::new(Vec::new())), &Config::default());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &engine,
            style,
            Cursor::new(input.to_vec()),
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn an_interactive_session_prompts_echoes_and_shows_values() {
        let (code, out, err) = session(b"1 + 2\nlet x = 5;\nx +\r1\rmissing\n", Style::Interactive);
        assert_eq!(code, 0);
        assert!(out.starts_with("rhai REPL"), "{out}");
        // The typed text is echoed after the prompt, then the value.
        assert!(out.contains("rhai> 1 + 2\n3\n"), "{out:?}");
        assert!(out.contains("rhai> x +\n  ... 1\n6\n"), "{out:?}");
        assert!(err.contains("Variable not found: missing (line 1"), "{err}");
        assert!(out.ends_with("rhai> \n"), "{out:?}");
    }

    #[test]
    fn interactive_editing_backspace_ctrl_c_and_ctrl_d() {
        let (code, out, _) = session(b"1 + 9\x7f2\nif true {\x03\n5\n\x04", Style::Interactive);
        assert_eq!(code, 0);
        assert!(out.contains("rhai> 1 + 9\x08 \x082\n3\n"), "{out:?}");
        // Ctrl-C abandoned the unfinished block, so `5` starts fresh.
        assert!(out.contains("^C\n"), "{out:?}");
        assert!(out.contains("rhai> 5\n5\n"), "{out:?}");
    }

    #[test]
    fn interactive_history_recall_with_bang_bang() {
        let (_, out, _) = session(
            b"6 * 7
!!
",
            Style::Interactive,
        );
        assert_eq!(
            out.matches(
                "42
"
            )
            .count(),
            2,
            "{out:?}"
        );
    }

    #[test]
    fn interactive_input_arriving_in_odd_chunks() {
        // One key per read, as on the console.
        struct OneByte(Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = 1.min(buf.len());
                self.0.read(&mut buf[..n])
            }
        }
        let engine = build_engine(Rc::new(StdHost::new(Vec::new())), &Config::default());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let input =
            io::BufReader::with_capacity(1, OneByte(Cursor::new(b"6 * 7\n:quit\n".to_vec())));
        let code = run(&engine, Style::Interactive, input, &mut out, &mut err);
        assert_eq!(code, 0);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("rhai> 6 * 7\n42\n"));
    }

    #[test]
    fn a_signal_interrupted_read_is_retried() {
        // The terminal read returns EINTR when a signal lands mid-wait.
        struct Flaky {
            interrupted: bool,
            data: Cursor<Vec<u8>>,
        }
        impl Read for Flaky {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.data.read(buf)
            }
        }
        let engine = build_engine(Rc::new(StdHost::new(Vec::new())), &Config::default());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let input = io::BufReader::new(Flaky {
            interrupted: false,
            data: Cursor::new(b"6 * 7\n".to_vec()),
        });
        let code = run(&engine, Style::Interactive, input, &mut out, &mut err);
        assert_eq!(code, 0);
        assert!(String::from_utf8(out).unwrap().contains("42\n"));
    }

    #[test]
    fn batch_mode_prints_only_results() {
        let (code, out, _) = session(
            b"6 * 7\nfn f() {\n  1\n}\nf()\n:quit\nignored\n",
            Style::Batch,
        );
        assert_eq!(code, 0);
        assert_eq!(out, "42\n1\n");
    }

    #[test]
    fn exit_sets_the_status_and_crlf_is_accepted() {
        let (code, out, _) = session(b"1 + 1\r\nexit(9)\r\n", Style::Batch);
        assert_eq!(code, 9);
        assert_eq!(out, "2\n");
    }

    #[test]
    fn bad_utf8_lines_are_reported_and_skipped() {
        let (code, out, err) = session(b"\xff\xfe\n1\n", Style::Batch);
        assert_eq!(code, 0);
        assert_eq!(out, "1\n");
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn a_missing_final_newline_still_runs() {
        let (_, out, _) = session(b"40 + 2", Style::Batch);
        assert_eq!(out, "42\n");
    }

    #[test]
    fn over_long_lines_are_discarded_without_buffering_them() {
        let mut input = vec![b'1'; MAX_LINE + 5000];
        input.extend_from_slice(b"\n2\n");
        let (code, out, err) = session(&input, Style::Batch);
        assert_eq!(code, 0);
        assert_eq!(out, "2\n");
        assert!(err.contains("discarded"), "{err}");
    }

    #[test]
    fn bounded_line_reader_edges() {
        let mut buf = Vec::new();
        let mut input = Cursor::new(b"ab\ncd".to_vec());
        assert_eq!(
            read_line_bounded(&mut input, 10, &mut buf).unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"ab\n");
        assert_eq!(
            read_line_bounded(&mut input, 10, &mut buf).unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"cd");
        assert_eq!(
            read_line_bounded(&mut input, 10, &mut buf).unwrap(),
            LineRead::Eof
        );
        // A line that fills the cap before its newline is too long.
        let mut input = Cursor::new(b"abcd\nnext\n".to_vec());
        assert_eq!(
            read_line_bounded(&mut input, 4, &mut buf).unwrap(),
            LineRead::TooLong
        );
        assert_eq!(
            read_line_bounded(&mut input, 8, &mut buf).unwrap(),
            LineRead::Line
        );
        assert_eq!(buf, b"next\n");
    }

    #[test]
    fn eof_inside_a_block_ends_the_session() {
        let (code, _, _) = session(b"if true {\n", Style::Batch);
        assert_eq!(code, 0);
    }
}

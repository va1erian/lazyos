//! The serial evidence: exactly one `TERM:OUT` per submitted command, the
//! first output line, found among the lines the screen completed (the shell's
//! echo of the command and bare prompts are skipped).

use lazyterm::Line;

/// A completed row only continues on the next one while a row-wide line is
/// this short of the evidence limit (a bound on the joined prefix).
const CARRY_MAX: usize = 16 * 1024;

/// Whether a completed grid line is only a shell prompt: `# `, `$ `, or
/// BusyBox's default `\w \$ ` form with the working directory first (`/ # `,
/// `/tmp $ `). A prompt followed by a typed command (`/ # rhai`) is not one.
///
/// The Terminal reports exactly one `TERM:OUT` per command, so a bare prompt
/// line must never be taken for that output: the shell emits one when the
/// next command was typed before its prompt appeared (a slow reap of the
/// previous pipeline), and the real output line would then go unreported.
pub fn is_prompt(line: &str) -> bool {
    let marks = |text: &str| text.chars().all(|c| matches!(c, '#' | '$' | '>' | ' '));
    // BusyBox's `\w` is an absolute path or `~`-relative, so anything else in
    // front of the mark (`a #`, `x $`) is ordinary output that merely ends
    // like a prompt.
    let is_cwd =
        |text: &str| (text.starts_with('/') || text.starts_with('~')) && !text.contains(' ');
    match line.trim().rsplit_once(' ') {
        None => marks(line),
        Some((cwd, mark)) => is_cwd(cwd) && marks(mark),
    }
}

/// The command in flight and the evidence state around it.
#[derive(Default)]
pub struct Evidence {
    /// The last submitted command and whether its first output line is still
    /// expected.
    last_cmd: Option<String>,
    awaiting_output: bool,
    /// The text of the full-width rows just completed. BusyBox's line editor
    /// breaks a command that reaches the last column with its own newline
    /// (no autowrap, so the engine cannot join the rows), and the echo of a
    /// long command arrives as two or more lines; remembering the full rows
    /// lets the echo be recognised whole.
    carry: String,
}

impl Evidence {
    /// A command was submitted (Enter): its first output line is next.
    pub fn submitted(&mut self, command: String) {
        self.last_cmd = Some(command);
        self.awaiting_output = true;
    }

    /// One line the screen completed. Returns the `TERM:OUT` payload when it
    /// is the awaited output line.
    pub fn line(&mut self, line: &Line) -> Option<String> {
        let marker = line.text.trim();
        let joined = format!("{}{}", self.carry, line.text);
        self.carry = if line.ends_at_margin && joined.len() <= CARRY_MAX {
            joined.clone()
        } else {
            String::new()
        };
        if marker.is_empty() || is_prompt(marker) {
            return None;
        }
        // Skip the shell's echo of the command it is about to run; the next
        // non-empty line is the output.
        if let Some(command) = &self.last_cmd {
            let echo = |text: &str| text.trim().ends_with(command.as_str());
            if self.awaiting_output && (echo(marker) || echo(&joined)) {
                return None;
            }
        }
        if self.awaiting_output {
            self.awaiting_output = false;
            return Some(marker.to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use lazyterm::Term;

    use super::*;

    /// A bare prompt, with or without the working directory, is a prompt; a
    /// prompt with a command after it, and any output line, is not.
    #[test]
    fn prompt_lines_with_a_working_directory_are_prompts() {
        for prompt in [
            "# ",
            "$",
            "> ",
            "/ #",
            "/ # ",
            "/tmp $ ",
            "/home/user $",
            "~ $",
        ] {
            assert!(is_prompt(prompt), "{prompt:?} should be a prompt");
        }
        for line in [
            "/ # rhai",
            "$ echo",
            "rhai>",
            "42",
            "a #",
            "x $",
            "cost 5 $",
            "HI",
            "rhai REPL: :help",
            "",
        ] {
            assert!(
                !is_prompt(line) || line.is_empty(),
                "{line:?} is not a prompt"
            );
        }
    }

    /// Feed `bytes` to `term` and return the `TERM:OUT` payloads.
    fn outs(evidence: &mut Evidence, term: &mut Term, bytes: &[u8]) -> Vec<String> {
        term.feed(bytes)
            .iter()
            .filter_map(|line| evidence.line(line))
            .collect()
    }

    #[test]
    fn one_out_per_command_after_the_echo() {
        let mut term = Term::new(80, 24);
        let mut evidence = Evidence::default();
        outs(&mut evidence, &mut term, b"/ # echo hi");
        evidence.submitted("echo hi".into());
        let got = outs(&mut evidence, &mut term, b"\r\nhi\r\nmore\r\n/ # ");
        assert_eq!(got, ["hi"], "the echo, the second line and the prompt are not it");
    }

    /// The Doom session's command is 75 columns: with the prompt it fills an
    /// 80-column row, and BusyBox breaks the row itself (`CR CR LF`), so the
    /// echo arrives as two lines.
    #[test]
    fn a_long_echo_broken_by_the_shell_is_recognised() {
        let command = format!("echo {}", "1234567890".repeat(9));
        let mut term = Term::new(80, 24);
        let mut evidence = Evidence::default();
        // The shell echoes while the command is typed: the first row ends at
        // the margin and is completed before Enter.
        let echo = format!("/ $ {command}");
        let (head, tail) = echo.split_at(80);
        outs(&mut evidence, &mut term, format!("{head}\r\r\n{tail}").as_bytes());
        evidence.submitted(command.clone());
        let got = outs(&mut evidence, &mut term, format!("\r\n{}\r\n/ $ ", &command[5..]).as_bytes());
        assert_eq!(got, [command[5..].to_string()], "the echo's tail is not the output");
    }

    #[test]
    fn an_output_line_that_ends_like_the_command_is_still_output() {
        let mut term = Term::new(80, 24);
        let mut evidence = Evidence::default();
        evidence.submitted("echo 10".into());
        let got = outs(&mut evidence, &mut term, b"/ # echo 10\r\n10\r\n");
        assert_eq!(got, ["10"]);
    }

    #[test]
    fn a_wrapped_output_line_is_reported_whole() {
        let mut term = Term::new(10, 6);
        let mut evidence = Evidence::default();
        evidence.submitted("echo x".into());
        let got = outs(&mut evidence, &mut term, b"/ # echo x\r\nabcdefghijklmno\r\n");
        assert_eq!(got, ["abcdefghijklmno"]);
    }
}

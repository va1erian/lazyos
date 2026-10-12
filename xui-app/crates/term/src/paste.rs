//! Making pasted or dropped text safe to hand to a program's input.

/// The bytes to write to the pty for pasted `text`. Newlines (`\r\n`, `\n`)
/// become `\r`; every other control character (ESC included, so a pasted
/// `ESC [ 201 ~` cannot end the bracket early or smuggle a sequence in),
/// DEL and the C1 controls are dropped, except TAB. With `bracketed`, the
/// text is wrapped in `ESC [ 200 ~` ... `ESC [ 201 ~`.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = String::with_capacity(text.len() + 12);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\r');
            }
            '\n' => out.push('\r'),
            '\t' => out.push('\t'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    if bracketed {
        format!("\x1b[200~{out}\x1b[201~").into_bytes()
    } else {
        out.into_bytes()
    }
}

/// How many lines `text` has once pasted: the number of newlines it holds,
/// plus one when it does not end with one.
pub fn line_count(text: &str) -> usize {
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let newlines = normalised.matches('\n').count();
    newlines + usize::from(!normalised.is_empty() && !normalised.ends_with('\n'))
}

/// Whether a paste needs the user's confirmation: it has a newline, so a
/// shell would run it, and the program did not ask for bracketed paste (a
/// program that did knows to treat it as text).
pub fn needs_confirmation(text: &str, bracketed: bool) -> bool {
    !bracketed && paste_bytes(text, false).contains(&b'\r')
}

/// `path` quoted for a POSIX shell: left alone when it holds only safe
/// characters, else wrapped in single quotes with embedded quotes escaped.
pub fn shell_quote(path: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c);
    if !path.is_empty() && path.chars().all(safe) {
        return path.to_string();
    }
    let sanitised: String = path.chars().filter(|c| !c.is_control()).collect();
    format!("'{}'", sanitised.replace('\'', "'\\''"))
}

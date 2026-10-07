//! Text the trusted prompt and the audit trail show (review of #659).
//!
//! The prompt must show what an administrator approves, all of it, and the
//! audit line must say what happened and nothing a caller forged. Every
//! value an asker chose goes through here first:
//!
//! * [`misleading`] characters (control, Unicode format such as the bidi
//!   overrides U+202A-202E and isolates U+2066-2069, U+200E/F, line and
//!   paragraph separators, and every space but U+0020) can make text read
//!   other than it is. Arguments that are typed by people and stored as
//!   they are (a `conf.set` text value, a power policy value, a package
//!   path) refuse them ([`plain`]); text that comes from elsewhere (a
//!   package's manifest) shows them escaped.
//! * [`shown`] escapes everything the prompt's font cannot draw faithfully
//!   (it has printable ASCII and Latin-1 only) as `\u{...}`, and `\` and `"`
//!   so a quoted value ends where it seems to.
//! * [`elide`] and [`elide_path`] shorten long text **in the middle** with
//!   a visible `...`, so the end of a path or a value is never what is lost.
//! * [`wrap`] breaks the summary into the prompt's lines and, if it still
//!   does not fit, ends the last line with a visible `...`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The visible mark of left-out text. ASCII: the prompt's font has no `…`.
pub const ELLIPSIS: &str = "...";

/// Unicode format characters (general category Cf) and the line and
/// paragraph separators (Zl, Zp).
pub fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD
        | 0x0600..=0x0605
        | 0x061C
        | 0x06DD
        | 0x070F
        | 0x0890..=0x0891
        | 0x08E2
        | 0x180E
        | 0x200B..=0x200F
        | 0x2028..=0x202E
        | 0x2060..=0x2064
        | 0x2066..=0x206F
        | 0xFEFF
        | 0xFFF9..=0xFFFB
        | 0x110BD
        | 0x110CD
        | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A
        | 0xE0001
        | 0xE0020..=0xE007F)
}

/// A character that can make text read other than it is: a control or
/// format character, or any space but U+0020.
pub fn misleading(c: char) -> bool {
    c.is_control() || is_format(c) || (c.is_whitespace() && c != ' ')
}

/// `text` holds no [`misleading`] character.
pub fn plain(text: &str) -> bool {
    !text.chars().any(misleading)
}

/// `text` holds no [`misleading`] character but line breaks and tabs: a
/// stored value with rows and columns (`sys/ui/menu` is `app\tlabel\n`
/// rows). Both are shown and audited escaped (`\n`, `\t`), so neither can
/// end a prompt line or an audit line; a carriage return stays refused.
pub fn plain_rows(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c != '\n' && c != '\t' && misleading(c))
}

/// A character the prompt's font draws as itself: printable ASCII and
/// Latin-1 (U+00A0, a space that is not one, and the soft hyphen excluded).
fn drawable(c: char) -> bool {
    matches!(c, ' '..='~' | '\u{a1}'..='\u{ff}') && c != '\u{ad}'
}

/// One character as the prompt shows it.
fn unit(c: char) -> String {
    match c {
        '\\' => String::from("\\\\"),
        '"' => String::from("\\\""),
        '\n' => String::from("\\n"),
        '\t' => String::from("\\t"),
        '\r' => String::from("\\r"),
        c if drawable(c) => String::from(c),
        c => format!("\\u{{{:x}}}", c as u32),
    }
}

/// `text` with every character the prompt could not draw faithfully, and
/// `\` and `"`, escaped. The result is printable ASCII and Latin-1 only.
pub fn shown(text: &str) -> String {
    text.chars().map(unit).collect()
}

/// [`shown`] at most `max` characters long: a longer text keeps its head and
/// its tail around [`ELLIPSIS`], and an escape is never cut in two.
pub fn elide(text: &str, max: usize) -> String {
    let units: Vec<String> = text.chars().map(unit).collect();
    elide_units(&units, max)
}

fn elide_units(units: &[String], max: usize) -> String {
    let total: usize = units.iter().map(|u| u.chars().count()).sum();
    if total <= max {
        return units.concat();
    }
    let budget = max.saturating_sub(ELLIPSIS.len());
    let mut head = String::new();
    let mut used = 0;
    for unit in units {
        let len = unit.chars().count();
        if used + len > budget.div_ceil(2) {
            break;
        }
        head.push_str(unit);
        used += len;
    }
    let mut tail: Vec<&str> = Vec::new();
    let mut tail_used = 0;
    for unit in units.iter().rev() {
        let len = unit.chars().count();
        if used + tail_used + len > budget {
            break;
        }
        tail.push(unit);
        tail_used += len;
    }
    tail.reverse();
    format!("{head}{ELLIPSIS}{}", tail.concat())
}

/// A `/`-separated path at most `max` characters long: its first segment and
/// as much of its end as fits, whole segments where they fit
/// (`sys/ui/.../demo`), else [`elide`]d.
pub fn elide_path(path: &str, max: usize) -> String {
    let full = shown(path);
    if full.chars().count() <= max {
        return full;
    }
    let segments: Vec<String> = path.split('/').map(shown).collect();
    if segments.len() >= 3 {
        // Keep the first segment and as many last ones as fit.
        let head = &segments[0];
        let mut tail: Vec<&str> = Vec::new();
        let mut len = head.chars().count() + 1 + ELLIPSIS.len();
        for segment in segments[1..].iter().rev() {
            let more = 1 + segment.chars().count();
            if len + more > max {
                break;
            }
            tail.push(segment);
            len += more;
        }
        if !tail.is_empty() {
            tail.reverse();
            return format!("{head}/{ELLIPSIS}/{}", tail.join("/"));
        }
    }
    elide(path, max)
}

/// A text value as the prompt shows it: quoted, [`shown`], a run of more
/// than three spaces counted instead of drawn (`\[12 spaces]`: blank space
/// could push the rest out of sight; a literal `\` is always doubled, so the
/// mark cannot be faked), and [`elide`]d to `max` with its full length named.
pub fn quoted(text: &str, max: usize) -> String {
    let mut units: Vec<String> = Vec::new();
    let mut spaces = 0usize;
    let flush = |units: &mut Vec<String>, spaces: &mut usize| {
        match *spaces {
            0 => {}
            1..=3 => units.extend((0..*spaces).map(|_| String::from(" "))),
            n => units.push(format!("\\[{n} spaces]")),
        }
        *spaces = 0;
    };
    for c in text.chars() {
        if c == ' ' {
            spaces += 1;
            continue;
        }
        flush(&mut units, &mut spaces);
        units.push(unit(c));
    }
    flush(&mut units, &mut spaces);
    let body = elide_units(&units, max);
    if body == units.concat() {
        format!("\"{body}\"")
    } else {
        format!("\"{body}\" ({} characters)", text.chars().count())
    }
}

/// `text` broken at spaces into at most `max` lines that each satisfy
/// `fits` (a word wider than a line is broken between characters). When it
/// takes more, the last line ends in [`ELLIPSIS`]: a cut is always visible.
pub fn wrap(text: &str, max: usize, fits: impl Fn(&str) -> bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            String::from(word)
        } else {
            format!("{line} {word}")
        };
        if fits(&candidate) {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(core::mem::take(&mut line));
        }
        for c in word.chars() {
            line.push(c);
            if !fits(&line) && line.chars().count() > 1 {
                line.pop();
                lines.push(core::mem::replace(&mut line, String::from(c)));
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        lines.truncate(max.max(1));
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && !fits(&format!("{last}{ELLIPSIS}")) {
                last.pop();
            }
            last.push_str(ELLIPSIS);
        }
    }
    lines
}

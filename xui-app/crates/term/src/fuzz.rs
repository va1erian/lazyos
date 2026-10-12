//! The seeded fuzz entry point: any bytes fed to a terminal of any size, in
//! any chunking, with resizes in between, must leave a consistent screen and
//! never panic.
//!
//! [`run`] is what a cargo-fuzz target calls; the in-tree test mutates the
//! seeds below with a seeded generator (`FUZZ_CASES`, `FUZZ_SEED` override the
//! run, as in the other fuzzed libraries). On LazyOS the app builds with
//! `panic = "abort"`, so a panic here is a crash there.

use crate::input::{mouse_report, Action, Button, Mods, MouseMode};
use crate::paste::{paste_bytes, shell_quote};
use crate::settings::{Settings, Value, KEYS};
use crate::{Point, SelectMode, Term};

/// Check every invariant the rest of the code relies on; panics on the first
/// one that fails.
pub fn check(term: &Term) {
    let state = &term.state;
    let (cols, rows) = (term.cols(), term.rows());
    assert!(state.row < rows && state.col < cols, "cursor off screen");
    assert!(state.top < state.bottom || rows == 1, "bad scroll region");
    assert!(state.bottom < rows, "region below the screen");
    assert_eq!(state.tabs.len(), cols, "tab stops");
    assert_eq!(state.screen.rows.len(), rows, "row count");
    assert!(state.history.is_consistent(), "history");
    assert_eq!(term.screen_top(), term.first_line() + term.scrollback_len() as u64);
    for line in term.first_line()..term.screen_top() + rows as u64 {
        assert!(term.line(line).is_some(), "line {line} missing");
    }
    assert!(term.line(term.screen_top() + rows as u64).is_none());
    if let Some((start, end)) = term.selection_range() {
        assert!(start <= end, "selection ends out of order");
        assert!(start.line >= term.first_line(), "selection below the history");
        assert!(end.line < term.screen_top() + rows as u64, "selection past the screen");
        assert!(term.selection_text().is_some());
        for line in start.line..=end.line.min(start.line + 4) {
            let (from, to) = term.selection_span(line).expect("a span inside the selection");
            assert!(from <= to && to < cols, "span {from}..={to}");
        }
    }
    if state.on_alternate() {
        assert!(state.top == 0 || state.bottom < rows);
    }
    for (index, row) in state.screen.rows.iter().enumerate() {
        assert_eq!(row.cells.len(), cols, "row {index} width");
        assert!(row.is_consistent(), "row {index} wide characters");
        for cell in &row.cells {
            assert!(usize::from(cell.attr.link) <= state.links.len(), "a link past the table");
        }
    }
    if let Some(title) = term.title() {
        assert!(title.chars().count() <= crate::MAX_TITLE && !title.chars().any(|c| c.is_control()), "title");
    }
    if let Some(cwd) = term.cwd() {
        assert!(cwd.path.len() <= crate::osc_meta::MAX_CWD && !cwd.path.chars().any(|c| c.is_control()), "cwd");
    }
    assert!(state.links.len() <= crate::links::MAX_LINKS);
}

/// The logical lines of the scrollback and screen up to and including the
/// cursor's (what a reflow must preserve), and whether the cursor's line was
/// seen to its end: a reflow that keeps the cursor on screen may crop the rest
/// of a long line below the bottom row.
fn through_cursor(term: &Term) -> (Vec<String>, bool) {
    let last = term.screen_top() + term.cursor().0 as u64;
    let mut out = Vec::new();
    let mut current = String::new();
    let mut complete = false;
    for line in term.first_line()..term.screen_top() + term.rows() as u64 {
        let row = term.line(line).expect("a line");
        current.extend(row.cells.iter().filter(|cell| cell.is_glyph()).map(|cell| cell.ch));
        if row.wrapped {
            let missing = term.cols().saturating_sub(row.cells.len());
            current.extend(std::iter::repeat_n(' ', missing));
        } else {
            out.push(current.trim_end().to_string());
            current.clear();
            // The cursor's logical line ends here: nothing below it counts.
            if line >= last {
                complete = true;
                break;
            }
        }
    }
    if !complete {
        out.push(current.trim_end().to_string());
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    (out, complete)
}

/// A reflow kept the text through the cursor's line (a cropped tail excepted).
fn assert_preserved(before: &(Vec<String>, bool), after: &(Vec<String>, bool), note: &str) {
    let (before, after_lines) = (&before.0, &after.0);
    if after.1 {
        assert_eq!(after_lines, before, "reflow changed the text {note}");
        return;
    }
    let n = after_lines.len();
    assert!(n <= before.len(), "reflow added text {note}");
    if n > 0 {
        assert_eq!(after_lines[..n - 1], before[..n - 1], "reflow changed the text {note}");
        assert!(before[n - 1].starts_with(&after_lines[n - 1]), "reflow changed the last line {note}");
    }
}

/// Feed `data` as: two size bytes, then chunks whose lengths and the resizes
/// between them are drawn from the data itself.
pub fn run(data: &[u8]) {
    let [first, second, rest @ ..] = data else {
        return;
    };
    let mut term = Term::new(1 + usize::from(*first) % 120, 1 + usize::from(*second) % 50);
    let mut at = 0;
    while at < rest.len() {
        let len = 1 + usize::from(rest[at]) % 48;
        let chunk = &rest[at..(at + len).min(rest.len())];
        at += len;
        let _ = term.feed(chunk);
        let _ = term.take_reply();
        check(&term);
        exercise(&mut term, chunk);
        check(&term);
        if chunk[0] % 17 == 0 {
            term.set_scrollback(usize::from(chunk[chunk.len() - 1]) % 40);
            check(&term);
        }
        if chunk[0] % 31 == 0 {
            let (cols, rows) = (
                1 + usize::from(chunk[chunk.len() - 1]) % 120,
                1 + usize::from(chunk[0]) % 50,
            );
            let before = (!term.on_alternate_screen()).then(|| through_cursor(&term));
            let kept = term.scrollback_len() + term.rows();
            let dbg_old = (term.cols(), term.rows(), term.cursor(), term.scrollback_len());
            term.resize(cols, rows);
            check(&term);
            // Reflow keeps every line up to the cursor's, unless the budget
            // evicted some or one column cannot hold a wide character.
            if let Some(before) = before {
                if cols >= 2 && term.scrollback_len() < term.state.history.limit() && kept > 0 {
                    assert_preserved(&before, &through_cursor(&term), &format!("{dbg_old:?} to {:?}", (cols, rows, term.cursor(), term.scrollback_len())));
                }
            }
        }
    }
}

/// Selection, paste and mouse operations driven by `chunk`'s bytes.
fn exercise(term: &mut Term, chunk: &[u8]) {
    let byte = |at: usize| usize::from(chunk[at % chunk.len()]);
    let span = (term.screen_top() - term.first_line()) as usize + term.rows();
    let at = |a: usize, b: usize| {
        Point::new(term.first_line() + (a * 7 % (span + 3)) as u64, b * 5 % (term.cols() + 3))
    };
    let modes = [SelectMode::Char, SelectMode::Word, SelectMode::Line, SelectMode::Block];
    let (p, q) = (at(byte(1), byte(2)), at(byte(3), byte(4)));
    match byte(0) % 11 {
        0 => term.select_begin(modes[byte(5) % 4], p),
        1 => {
            term.select_begin(modes[byte(5) % 4], p);
            term.select_update(q);
        }
        2 => term.select_update(q),
        3 => term.select_extend(q),
        4 => term.select_all(),
        5 => term.select_clear(),
        6 => {
            let text = String::from_utf8_lossy(chunk).into_owned();
            let bracketed = byte(1) % 2 == 0;
            let out = paste_bytes(&text, bracketed);
            let inner: &[u8] = if bracketed { &out[6..out.len() - 6] } else { &out };
            assert!(inner.iter().all(|&b| b >= 0x20 && b != 0x7f || b == 9 || b == 13));
            assert!(!shell_quote(&text).is_empty());
        }
        7 => {
            let modes = [MouseMode::Off, MouseMode::Click, MouseMode::Drag, MouseMode::Motion];
            let buttons = [None, Some(Button::Left), Some(Button::Middle), Some(Button::Right), Some(Button::WheelUp), Some(Button::WheelDown)];
            let actions = [Action::Press, Action::Release, Action::Motion];
            let mods = Mods { shift: byte(7) & 1 == 1, alt: byte(7) & 2 == 2, ctrl: byte(7) & 4 == 4 };
            let report = mouse_report(modes[byte(1) % 4], byte(2) % 2 == 0, buttons[byte(3) % 6], actions[byte(4) % 3], mods, byte(5) * 40, byte(6) * 40);
            assert!(report.is_none_or(|bytes| bytes.starts_with(b"\x1b[")));
        }
        8 => {
            // Settings from arbitrary values: always within their bounds.
            let mut settings = Settings::default();
            let text = String::from_utf8_lossy(&chunk[1..]).into_owned();
            let key = KEYS[byte(1) % KEYS.len()];
            let value = match byte(2) % 3 {
                0 => Value::Bool(byte(3) % 2 == 0),
                1 => Value::Int(i64::from_le_bytes(std::array::from_fn(|i| byte(3 + i) as u8))),
                _ => Value::Text(text),
            };
            settings.apply(key, &value);
            assert!((6..=72).contains(&settings.size));
            assert!((100..=200).contains(&settings.line_spacing));
            assert!(settings.scrollback <= 200_000);
            assert!(settings.family.len() <= 64 * 4 && !settings.family.is_empty());
        }
        9 => {
            // Palette operations and the box-drawing geometry.
            let c = char::from_u32(0x2500 + byte(1) as u32 % 0xa0).unwrap();
            let size = 1 + (byte(2) % 30) as i32;
            let cell = crate::boxdraw::Box2::new(0, 0, size, size * 2);
            for (b, _) in crate::boxdraw::geometry(c, cell, 1 + (byte(3) % 4) as i32).unwrap_or_default() {
                assert!(b.x0 >= 0 && b.y0 >= 0 && b.x1 <= cell.x1 && b.y1 <= cell.y1);
            }
            let _ = term.palette().resolve(crate::Color::Indexed(byte(4) as u8), true, true, true);
        }
        _ => {
            let _ = term.selection_text();
        }
    }
}

/// A small xorshift generator: deterministic per seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

/// Typical terminal traffic the mutations start from: a shell prompt, an
/// editor redraw, wide characters, a scroll region, colours.
pub const SEEDS: &[&[u8]] = &[
    // T5: titles, the working directory, hyperlinks, modes and attributes.
    b"\x20\x10\x1b]0;a title\x07\x1b]2;x;y;z\x1b\\\x1b]7;file://h/a%20b\x07\x1b]8;id=1;http://a;b\x07link\x1b]8;;\x07 text\x1b[?1;1004;2026h\x1b=\x1b[>c\x1b[c\x1b[?2026l\x1b>\x1b[?1l",
    b"\x20\x10\x1b]8;;\x1b\\x\x1b]8;a:b;u\x1b[1;31my\x1b[0mz\x1b]8;;\x07\x1b]0;\x07\x1b]7;file:///%00\x07\x1b]2;\xff\x07\x1bc",
    b"\x20\x08\x1b]4;1;rgb:ff/00/00\x07\x1b]4;1;?\x07\x1b]10;#fff\x1b\\\x1b]11;?\x07\x1b]104\x07\x1b[38;2;1;2;3m\x1b[48;5;200mx\x1bc",
    b"\x08\x05a\nb\nc\nd\ne\nf\n\x1b[3J\x1b[1;3r\x1b[3;1H\n\n\x1bM\x1b[S\x1b[?1049h\n\n\n\n\n\x1b[?1049l\x1bc\n\n\n\n",
    b"\x4f\x17/ # echo hi\r\nhi\r\n/ # ",
    b"\x4f\x17\x1b[?1049h\x1b[H\x1b[2Jpager\x1b[5;1H:\x1b[?1049l",
    b"\x4f\x17\x1b[2;10r\x1b[10;1Hone\ntwo\ntwo\x1b[1;24r\x1bM\x1bM",
    b"\x4f\x17\x1b[1;31;42mred\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[48:5:200mx\x1b[m",
    "\x28\x0a\u{4e2d}\u{6587}\u{1f600} wide \u{4e2d}\x1b[3G\x1b[1P\x1b[@X".as_bytes(),
    b"\x14\x08abcdefghijklmnopqrstuvwxyz\x1b[3L\x1b[2M\x1b[5X\x1b7\x1b[H\x1b8",
    b"\x4f\x17\x1b[6n\x1b[c\x1b[5n\x1b[?25l\x1b[?7l\x1b[?7hlong long long \tline",
    b"\x4f\x17\t\x1bH\tx\x1b[g\t\x1b[3g\x1b[I\x1b[2Z\x1b#8\x1bc",
];

/// Tokens a mutation splices in: the values parsers trip over.
const TOKENS: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1b[?",
    b"999999999",
    b"65535",
    b"0;0",
    b";",
    b":",
    b"r",
    b"J",
    b"K",
    b"L",
    b"M",
    b"@",
    b"P",
    b"X",
    b"m",
    b"h",
    b"l",
    b"\n",
    b"\r",
    b"\t",
    "\u{4e2d}".as_bytes(),
    b"\xff",
    b"\xe4\xb8",
];

/// `seed` with a few random edits: flipped bytes, a cut, a duplicated run, a
/// token over a few bytes.
pub fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut data = seed.to_vec();
    for _ in 0..=rng.below(6) {
        if data.is_empty() {
            data.push(rng.next_u64() as u8);
            continue;
        }
        let at = rng.below(data.len());
        match rng.below(5) {
            0 => data[at] ^= 1 << rng.below(8),
            1 => data[at] = rng.next_u64() as u8,
            2 => data.truncate(at.max(data.len() / 2)),
            3 => {
                let len = rng.below(32).min(data.len() - at);
                let run = data[at..at + len].to_vec();
                let to = rng.below(data.len());
                data.splice(to..to, run);
            }
            _ => {
                let token = TOKENS[rng.below(TOKENS.len())];
                let end = (at + rng.below(4)).min(data.len());
                data.splice(at..end, token.iter().copied());
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_number(name: &str, default: u64) -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|v| {
                let v = v.trim();
                match v.strip_prefix("0x") {
                    Some(hex) => u64::from_str_radix(hex, 16).ok(),
                    None => v.parse().ok(),
                }
            })
            .unwrap_or(default)
    }

    #[test]
    fn the_seeds_themselves_are_consistent() {
        for seed in SEEDS {
            run(seed);
        }
    }

    #[test]
    fn seeded_mutations_never_panic() {
        let cases = env_number("FUZZ_CASES", 2000);
        let seed = env_number("FUZZ_SEED", 0x7e44_0017);
        let mut rng = Rng::new(seed);
        for case in 0..cases {
            let base = SEEDS[rng.below(SEEDS.len())];
            let data = mutate(&mut rng, base);
            let outcome = std::panic::catch_unwind(|| run(&data));
            assert!(
                outcome.is_ok(),
                "case {case} panicked; replay with FUZZ_SEED=0x{seed:x}"
            );
        }
    }

    /// Pure noise, including chunk and size bytes of every value.
    #[test]
    fn seeded_noise_never_panics() {
        let mut rng = Rng::new(env_number("FUZZ_SEED", 0x7e44_0017) ^ 0xdead);
        for _ in 0..env_number("FUZZ_CASES", 2000) / 4 {
            let len = rng.below(600);
            let data: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
            run(&data);
        }
    }
}

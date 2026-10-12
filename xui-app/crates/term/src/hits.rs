//! What is under a cell that can be opened: an OSC 8 hyperlink as the program
//! gave it, or a URL found in the text of the logical line.

use crate::logical::{read, start_of};
use crate::urls::find_urls;
use crate::{Point, Term};

/// A link under a cell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkHit {
    /// Where it goes.
    pub uri: String,
    /// What the screen shows for it. For a detected URL this is `uri`; an
    /// OSC 8 link may show other text, which the Terminal shows the target
    /// for before opening.
    pub text: String,
    /// The first and last cell it covers (inclusive).
    pub start: Point,
    pub end: Point,
    /// The program made it (OSC 8), rather than a pattern in the text.
    pub explicit: bool,
}

impl Term {
    /// The link at (`line`, `col`), if there is one.
    pub fn link_hit_at(&self, line: u64, col: usize) -> Option<LinkHit> {
        self.line(line)?;
        let logical = read(self, start_of(self, line))?;
        let chars = &logical.chars;
        let at = chars
            .iter()
            .position(|p| p.line == line && p.col <= col && col < p.col + usize::from(p.width))?;
        let span = |a: usize, b: usize| {
            let (first, last) = (chars[a], chars[b - 1]);
            (
                Point::new(first.line, first.col),
                Point::new(last.line, last.col + usize::from(last.width) - 1),
                chars[a..b].iter().map(|p| p.ch).collect::<String>(),
            )
        };
        let id = chars[at].link;
        if let Some(uri) = self.state.links.uri(id) {
            let mut a = at;
            while a > 0 && chars[a - 1].link == id {
                a -= 1;
            }
            let mut b = at + 1;
            while b < chars.len() && chars[b].link == id {
                b += 1;
            }
            let (start, end, text) = span(a, b);
            return Some(LinkHit { uri: uri.to_string(), text, start, end, explicit: true });
        }
        let text: Vec<char> = chars.iter().map(|p| p.ch).collect();
        let (a, b) = find_urls(&text).into_iter().find(|&(a, b)| a <= at && at < b)?;
        let (start, end, text) = span(a, b);
        Some(LinkHit { uri: text.clone(), text, start, end, explicit: false })
    }

    /// The scrollback and the screen as plain text: one line per logical line
    /// (wrapped rows joined), trailing blanks trimmed, `\n` between them, and
    /// no trailing empty lines.
    pub fn dump_text(&self) -> String {
        let mut out = String::new();
        let end = self.screen_top() + self.rows() as u64;
        let mut at = self.first_line();
        while at < end {
            let Some(logical) = read(self, at) else { break };
            out.extend(logical.chars.iter().map(|p| p.ch));
            out.push('\n');
            at = logical.last + 1;
        }
        out.truncate(out.trim_end_matches('\n').len());
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detected_url_is_found_from_any_of_its_cells() {
        let mut t = Term::new(30, 4);
        t.feed(b"see http://example.com/x, ok\r\n");
        let line = t.screen_top();
        let hit = t.link_hit_at(line, 8).unwrap();
        assert_eq!(hit.uri, "http://example.com/x");
        assert!(!hit.explicit && hit.text == hit.uri);
        assert_eq!((hit.start.col, hit.end.col), (4, 23));
        for col in 4..=23 {
            assert!(t.link_hit_at(line, col).is_some(), "col {col}");
        }
        assert!(t.link_hit_at(line, 3).is_none() && t.link_hit_at(line, 24).is_none());
        assert!(t.link_hit_at(line, 29).is_none());
        assert!(t.link_hit_at(line + 99, 0).is_none());
    }

    #[test]
    fn a_url_that_wraps_is_one_link() {
        let mut t = Term::new(10, 5);
        t.feed(b"go http://example.com/long/path now\r\n");
        let line = t.screen_top();
        let a = t.link_hit_at(line, 5).unwrap();
        let b = t.link_hit_at(line + 2, 1).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.uri, "http://example.com/long/path");
        assert_ne!(a.start.line, a.end.line);
    }

    #[test]
    fn an_osc_8_link_is_as_given_and_may_show_other_text() {
        let mut t = Term::new(40, 3);
        t.feed(b"x \x1b]8;;https://target.example/\x07click here\x1b]8;;\x07 y\r\n");
        let line = t.screen_top();
        let hit = t.link_hit_at(line, 4).unwrap();
        assert!(hit.explicit);
        assert_eq!(hit.uri, "https://target.example/");
        assert_eq!(hit.text, "click here");
        assert_eq!((hit.start.col, hit.end.col), (2, 11));
        assert!(t.link_hit_at(line, 12).is_none());
        // A URL shown as the text of its own link is the same thing.
        t.feed(b"\x1b]8;;http://a.b/\x07http://a.b/\x1b]8;;\x07\r\n");
        let hit = t.link_hit_at(line + 1, 3).unwrap();
        assert!(hit.explicit && hit.text == hit.uri);
    }

    #[test]
    fn a_wide_character_cell_hits_its_link() {
        let mut t = Term::new(40, 3);
        t.feed("\u{4e2d} http://x.example/".as_bytes());
        let line = t.screen_top();
        assert!(t.link_hit_at(line, 0).is_none());
        assert!(t.link_hit_at(line, 4).is_some());
    }

    #[test]
    fn the_dump_is_the_text_of_history_and_screen() {
        let mut t = Term::new(5, 3);
        for n in 0..4 {
            t.feed(format!("line{n}x\r\n").as_bytes());
        }
        t.feed(b"end");
        let dump = t.dump_text();
        assert_eq!(dump, "line0x\nline1x\nline2x\nline3x\nend\n");
        assert_eq!(Term::new(5, 3).dump_text(), "\n");
    }
}

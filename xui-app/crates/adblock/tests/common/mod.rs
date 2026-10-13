//! Test support: an independent parser/matcher for the URL-filter subset WebKit accepts
//! (so an emitted pattern WebKit would refuse fails a test here, not a whole list on a
//! device), an Adblock Plus matcher written without the converter's code, a validator
//! for a rule list, and a small seeded RNG.

#![allow(dead_code)]

use serde_json::Value;

// ---- the URL-filter subset ------------------------------------------------------------

#[derive(Debug)]
enum Atom {
    Char(u8),
    Any,
    Class { negated: bool, set: [bool; 128] },
    Group(Vec<Item>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quant {
    One,
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
}

#[derive(Debug)]
struct Item {
    atom: Atom,
    quant: Quant,
}

/// A parsed URL filter. `parse` refuses everything WebKit's `URLFilterParser` refuses
/// and, deliberately, a little more (a bare `]` or `}`), because the converter must
/// never emit those.
#[derive(Debug)]
pub struct UrlFilter {
    bol: bool,
    eol: bool,
    items: Vec<Item>,
    case_sensitive: bool,
}

pub fn parse_url_filter(src: &str, case_sensitive: bool) -> Result<UrlFilter, String> {
    if !src.is_ascii() || src.is_empty() {
        return Err("empty or non-ASCII".into());
    }
    let b = src.as_bytes();
    let mut pos = 0;
    let bol = b[0] == b'^';
    if bol {
        pos = 1;
    }
    let mut eol = false;
    let items = parse_seq(b, &mut pos, 0, &mut eol, case_sensitive)?;
    if pos != b.len() {
        return Err(format!("trailing input at {pos}"));
    }
    Ok(UrlFilter {
        bol,
        eol,
        items,
        case_sensitive,
    })
}

fn parse_seq(
    b: &[u8],
    pos: &mut usize,
    depth: usize,
    eol: &mut bool,
    cs: bool,
) -> Result<Vec<Item>, String> {
    let mut items: Vec<Item> = Vec::new();
    while *pos < b.len() {
        let c = b[*pos];
        let atom = match c {
            b')' => {
                if depth == 0 {
                    return Err("unbalanced )".into());
                }
                return Ok(items);
            }
            b'(' => {
                *pos += 1;
                if b.get(*pos) == Some(&b'?') || b.get(*pos) == Some(&b')') {
                    return Err("group form".into());
                }
                let mut inner_eol = false;
                let inner = parse_seq(b, pos, depth + 1, &mut inner_eol, cs)?;
                if inner_eol || b.get(*pos) != Some(&b')') {
                    return Err("bad group".into());
                }
                Atom::Group(inner)
            }
            b'[' => {
                *pos += 1;
                parse_class(b, pos)?
            }
            b'.' => Atom::Any,
            b'$' => {
                if *pos != b.len() - 1 || depth != 0 {
                    return Err("misplaced $".into());
                }
                *eol = true;
                *pos += 1;
                return Ok(items);
            }
            b'^' => return Err("misplaced ^".into()),
            b'*' | b'+' | b'?' => return Err("dangling quantifier".into()),
            b'|' | b'{' | b'}' | b']' => return Err(format!("unsupported {}", c as char)),
            b'\\' => {
                *pos += 1;
                let Some(&n) = b.get(*pos) else {
                    return Err("trailing backslash".into());
                };
                if n.is_ascii_alphanumeric() {
                    return Err("builtin class".into());
                }
                Atom::Char(n)
            }
            c => Atom::Char(c),
        };
        *pos += 1;
        let quant = match b.get(*pos) {
            Some(b'*') => Quant::ZeroOrMore,
            Some(b'+') => Quant::OneOrMore,
            Some(b'?') => Quant::ZeroOrOne,
            _ => Quant::One,
        };
        if quant != Quant::One {
            *pos += 1;
            if matches!(b.get(*pos), Some(b'*' | b'+' | b'?')) {
                return Err("lazy or doubled quantifier".into());
            }
        }
        items.push(Item { atom, quant });
    }
    if depth != 0 {
        return Err("unclosed group".into());
    }
    Ok(items)
}

fn parse_class(b: &[u8], pos: &mut usize) -> Result<Atom, String> {
    let mut negated = false;
    if b.get(*pos) == Some(&b'^') {
        negated = true;
        *pos += 1;
    }
    let mut set = [false; 128];
    let mut members: Vec<u8> = Vec::new();
    let start = *pos;
    loop {
        let Some(&c) = b.get(*pos) else {
            return Err("unclosed class".into());
        };
        match c {
            b']' if *pos > start => break,
            b']' | b'[' => return Err("class form".into()),
            b'\\' => {
                *pos += 1;
                match b.get(*pos) {
                    Some(&n) if !n.is_ascii_alphanumeric() => members.push(n),
                    _ => return Err("class escape".into()),
                }
            }
            c => members.push(c),
        }
        *pos += 1;
    }
    // Ranges: `a-z`, with a `-` first or last meaning itself.
    let mut i = 0;
    while i < members.len() {
        if i + 2 < members.len() && members[i + 1] == b'-' {
            let (lo, hi) = (members[i], members[i + 2]);
            if lo > hi {
                return Err("reversed range".into());
            }
            for ch in lo..=hi {
                set[ch as usize] = true;
            }
            i += 3;
        } else {
            set[members[i] as usize] = true;
            i += 1;
        }
    }
    Ok(Atom::Class { negated, set })
}

impl UrlFilter {
    /// Whether the pattern is found in `url`, the way WebKit searches a URL.
    pub fn is_match(&self, url: &str) -> bool {
        let text = url.as_bytes();
        let last_start = if self.bol { 0 } else { text.len() };
        (0..=last_start).any(|start| {
            self.seq(&self.items, text, start, &mut |end| {
                !self.eol || end == text.len()
            })
        })
    }

    fn seq(
        &self,
        items: &[Item],
        text: &[u8],
        pos: usize,
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let Some((first, rest)) = items.split_first() else {
            return k(pos);
        };
        let (min, max) = match first.quant {
            Quant::One => (1, 1),
            Quant::ZeroOrOne => (0, 1),
            Quant::ZeroOrMore => (0, usize::MAX),
            Quant::OneOrMore => (1, usize::MAX),
        };
        self.repeat(first, 0, (min, max), text, pos, rest, k)
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        item: &Item,
        count: usize,
        (min, max): (usize, usize),
        text: &[u8],
        pos: usize,
        rest: &[Item],
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        if count < max {
            let more = self.atom(&item.atom, text, pos, &mut |next| {
                if next == pos && count >= min {
                    return false;
                }
                self.repeat(item, count + 1, (min, max), text, next, rest, k)
            });
            if more {
                return true;
            }
        }
        count >= min && self.seq(rest, text, pos, k)
    }

    fn atom(&self, atom: &Atom, text: &[u8], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match atom {
            Atom::Group(items) => self.seq(items, text, pos, k),
            Atom::Any => pos < text.len() && k(pos + 1),
            Atom::Char(c) => pos < text.len() && self.same(text[pos], *c) && k(pos + 1),
            Atom::Class { negated, set } => {
                if pos >= text.len() {
                    return false;
                }
                let ch = text[pos];
                let mut hit = ch < 128 && set[ch as usize];
                if !self.case_sensitive && ch < 128 {
                    hit |= set[ch.to_ascii_lowercase() as usize]
                        || set[ch.to_ascii_uppercase() as usize];
                }
                hit != *negated && k(pos + 1)
            }
        }
    }

    fn same(&self, a: u8, b: u8) -> bool {
        if self.case_sensitive {
            a == b
        } else {
            a.eq_ignore_ascii_case(&b)
        }
    }
}

// ---- an Adblock Plus matcher, independent of the converter -----------------------------

enum Tok {
    Lit(u8),
    Star,
    Sep,
}

/// Whether Adblock Plus pattern `pattern` matches `url`. A trailing `^` needs a
/// separator character here, which is what the converter implements (and the one
/// documented way it blocks less than Adblock Plus: `end_sep_ok` is the real rule).
pub fn abp_matches(pattern: &str, url: &str, match_case: bool, end_sep_ok: bool) -> bool {
    let p = pattern.as_bytes();
    let (domain, start) = if p.starts_with(b"||") {
        (true, false)
    } else {
        (false, p.starts_with(b"|"))
    };
    let mut body = &p[if domain {
        2
    } else if start {
        1
    } else {
        0
    }..];
    let end = body.last() == Some(&b'|');
    if end {
        body = &body[..body.len() - 1];
    }
    let toks: Vec<Tok> = body
        .iter()
        .map(|&c| match c {
            b'*' => Tok::Star,
            b'^' => Tok::Sep,
            c => Tok::Lit(c),
        })
        .collect();
    let u = url.as_bytes();
    let starts: Vec<usize> = if start {
        vec![0]
    } else if domain {
        let host_start = url.find("://").map(|i| i + 3).unwrap_or(0);
        let host_end = u[host_start..]
            .iter()
            .position(|c| matches!(c, b'/' | b'?' | b'#' | b':'))
            .map(|i| host_start + i)
            .unwrap_or(u.len());
        (host_start..host_end)
            .filter(|&i| i == host_start || u[i - 1] == b'.')
            .collect()
    } else {
        (0..=u.len()).collect()
    };
    starts
        .into_iter()
        .any(|s| go(&toks, u, s, end, match_case, end_sep_ok))
}

fn is_separator(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

fn go(toks: &[Tok], u: &[u8], pos: usize, end: bool, case: bool, end_sep_ok: bool) -> bool {
    let Some((first, rest)) = toks.split_first() else {
        return !end || pos == u.len();
    };
    match first {
        Tok::Lit(c) => {
            pos < u.len()
                && (if case {
                    u[pos] == *c
                } else {
                    u[pos].eq_ignore_ascii_case(c)
                })
                && go(rest, u, pos + 1, end, case, end_sep_ok)
        }
        Tok::Sep => {
            if pos == u.len() {
                end_sep_ok && go(rest, u, pos, end, case, end_sep_ok)
            } else {
                is_separator(u[pos]) && go(rest, u, pos + 1, end, case, end_sep_ok)
            }
        }
        Tok::Star => (pos..=u.len()).any(|p| go(rest, u, p, end, case, end_sep_ok)),
    }
}

// ---- rule-list validation ------------------------------------------------------------

const RESOURCE_TYPES: [&str; 16] = [
    "document",
    "top-document",
    "child-document",
    "image",
    "style-sheet",
    "script",
    "font",
    "raw",
    "svg-document",
    "media",
    "popup",
    "ping",
    "fetch",
    "websocket",
    "other",
    "csp-report",
];

/// Checks one converted list the way WebKit's parser would, and returns its rules.
pub fn check_list(json: &str, max_rules: usize) -> Vec<Value> {
    let value: Value = serde_json::from_str(json).expect("a list is valid JSON");
    let rules = value.as_array().expect("a list is an array").clone();
    assert!(
        !rules.is_empty() && rules.len() <= max_rules,
        "{} rules",
        rules.len()
    );
    for rule in &rules {
        check_rule(rule);
    }
    rules
}

fn check_rule(rule: &Value) {
    let trigger = rule["trigger"].as_object().expect("trigger");
    let action = rule["action"]["type"].as_str().expect("action type");
    assert!(
        matches!(action, "block" | "ignore-previous-rules"),
        "{action}"
    );

    let case = trigger
        .get("url-filter-is-case-sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let filter = trigger["url-filter"].as_str().expect("url-filter");
    if filter != ".*" {
        parse_url_filter(filter, case)
            .unwrap_or_else(|e| panic!("WebKit would refuse {filter:?}: {e}"));
    }
    if let Some(types) = trigger.get("resource-type") {
        let types = types.as_array().expect("resource-type array");
        assert!(!types.is_empty());
        for t in types {
            assert!(RESOURCE_TYPES.contains(&t.as_str().unwrap()), "{t}");
        }
    }
    if let Some(load) = trigger.get("load-type") {
        let load = load.as_array().unwrap();
        assert_eq!(load.len(), 1);
        assert!(matches!(
            load[0].as_str(),
            Some("first-party" | "third-party")
        ));
    }
    let conditions = ["if-domain", "unless-domain", "if-top-url"]
        .iter()
        .filter(|k| trigger.contains_key(**k))
        .count();
    assert!(conditions <= 1, "one condition per trigger");
    for key in ["if-domain", "unless-domain"] {
        if let Some(domains) = trigger.get(key) {
            for d in domains.as_array().unwrap() {
                let d = d.as_str().unwrap();
                let name = d
                    .strip_prefix('*')
                    .expect("subdomains are matched with a *");
                assert!(
                    !name.is_empty()
                        && name.bytes().all(|b| b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || b == b'.'
                            || b == b'-'),
                    "{d}"
                );
            }
        }
    }
    if let Some(urls) = trigger.get("if-top-url") {
        for u in urls.as_array().unwrap() {
            parse_url_filter(u.as_str().unwrap(), case).expect("top URL filter");
        }
    }
}

// ---- seeded randomness -----------------------------------------------------------------

/// xorshift64*: enough to vary inputs, and a printed seed reproduces a failure.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// `FUZZ_CASES` (the repository's convention) or a default.
pub fn fuzz_cases(default: usize) -> usize {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `FUZZ_SEED=0x...` replays a failure.
pub fn fuzz_seed(default: u64) -> u64 {
    std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(default)
}

//! An Adblock Plus matcher written without the converter's code: the reference of the
//! differential tests.

#![allow(dead_code)]

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

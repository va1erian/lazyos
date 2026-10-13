//! One line of a filter list: a comment, an element-hiding rule, or a network filter
//! split into its pattern and options.

/// One `$option`: `name`, `~name` or `name=value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opt<'a> {
    /// Lower case.
    pub name: String,
    pub negated: bool,
    pub value: Option<&'a str>,
}

/// A network filter: `[@@]pattern[$options]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Filter<'a> {
    pub exception: bool,
    pub pattern: &'a str,
    pub options: Vec<Opt<'a>>,
}

pub(crate) enum Line<'a> {
    /// Blank, `!` comment or the list header.
    Skip,
    /// Element hiding: not converted in v1.
    Cosmetic,
    Network(Filter<'a>),
}

pub(crate) fn parse_line(raw: &str) -> Line<'_> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('!') || (line.starts_with('[') && line.ends_with(']')) {
        return Line::Skip;
    }
    if is_cosmetic(line) {
        return Line::Cosmetic;
    }
    let (exception, body) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (pattern, options) = split_options(body);
    Line::Network(Filter {
        exception,
        pattern,
        options,
    })
}

/// `domains##selector`, `#@#`, `#?#`, `#$#`, `#%#` and the `@` forms of those. A `#` in
/// a URL fragment is not one: the part before it must look like a domain list.
fn is_cosmetic(line: &str) -> bool {
    let Some(at) = line.find('#') else {
        return false;
    };
    let (prefix, rest) = line.split_at(at);
    let domains_only = prefix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b',' | b'~' | b'*' | b'_' | b'-'));
    if !domains_only {
        return false;
    }
    let mut after = rest[1..].bytes().peekable();
    if after.peek() == Some(&b'@') {
        after.next();
    }
    if matches!(after.peek(), Some(b'?' | b'$' | b'%')) {
        after.next();
    }
    after.next() == Some(b'#')
}

/// Splits at the last `$` whose tail reads as an option list, so a `$` inside a regex
/// (`/ads$/`) stays in the pattern.
fn split_options(body: &str) -> (&str, Vec<Opt<'_>>) {
    if let Some(at) = body.rfind('$') {
        let tail = &body[at + 1..];
        if let Some(options) = parse_options(tail) {
            return (&body[..at], options);
        }
    }
    (body, Vec::new())
}

fn parse_options(tail: &str) -> Option<Vec<Opt<'_>>> {
    if tail.is_empty() {
        return None;
    }
    let mut options = Vec::new();
    for token in tail.split(',') {
        let (token, negated) = match token.strip_prefix('~') {
            Some(rest) => (rest, true),
            None => (token, false),
        };
        let (name, value) = match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        };
        let name_ok = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !name_ok {
            return None;
        }
        options.push(Opt {
            name: name.to_ascii_lowercase(),
            negated,
            value,
        });
    }
    Some(options)
}

impl Filter<'_> {
    /// What `$badfilter` matches on: the filter without that option, with the options in a
    /// fixed order. Two lines with the same key are the same filter.
    pub fn key(&self) -> String {
        let mut names: Vec<String> = self
            .options
            .iter()
            .filter(|o| o.name != "badfilter")
            .map(|o| {
                let mut s = String::new();
                if o.negated {
                    s.push('~');
                }
                s.push_str(&o.name);
                if let Some(v) = o.value {
                    s.push('=');
                    s.push_str(v);
                }
                s
            })
            .collect();
        names.sort();
        format!(
            "{}{}${}",
            if self.exception { "@@" } else { "" },
            self.pattern,
            names.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(line: &str) -> Filter<'_> {
        match parse_line(line) {
            Line::Network(f) => f,
            _ => panic!("not a network filter: {line}"),
        }
    }

    #[test]
    fn comments_and_headers_are_skipped() {
        for line in ["", "   ", "! Title: EasyList", "[Adblock Plus 2.0]"] {
            assert!(matches!(parse_line(line), Line::Skip), "{line:?}");
        }
    }

    #[test]
    fn element_hiding_is_cosmetic() {
        for line in [
            "##.ad",
            "example.com##.ad",
            "~a.com,b.com##div[id=\"x\"]",
            "example.com#@#.ad",
            "example.com#?#div:has(> .ad)",
            "example.com##+js(abort-on-property-read, x)",
            "example.com#$#body { overflow: auto }",
        ] {
            assert!(matches!(parse_line(line), Line::Cosmetic), "{line:?}");
        }
    }

    #[test]
    fn a_fragment_is_not_element_hiding() {
        let f = network("||example.com/page#section");
        assert_eq!(f.pattern, "||example.com/page#section");
        let f = network("/ads#top^");
        assert_eq!(f.pattern, "/ads#top^");
    }

    #[test]
    fn options_split_at_the_last_dollar() {
        let f = network("@@||example.com^$script,~third-party,domain=a.com|b.com");
        assert!(f.exception);
        assert_eq!(f.pattern, "||example.com^");
        let names: Vec<_> = f
            .options
            .iter()
            .map(|o| (o.name.as_str(), o.negated, o.value))
            .collect();
        assert_eq!(
            names,
            [
                ("script", false, None),
                ("third-party", true, None),
                ("domain", false, Some("a.com|b.com"))
            ]
        );
    }

    #[test]
    fn a_dollar_in_a_regex_stays_in_the_pattern() {
        let f = network("/ads$/");
        assert_eq!(f.pattern, "/ads$/");
        assert!(f.options.is_empty());
        let f = network("/banner[0-9]+$/$script");
        assert_eq!(f.pattern, "/banner[0-9]+$/");
        assert_eq!(f.options.len(), 1);
    }

    #[test]
    fn badfilter_key_ignores_option_order_and_the_option_itself() {
        let a = network("||x.com^$script,third-party");
        let b = network("||x.com^$third-party,script,badfilter");
        assert_eq!(a.key(), b.key());
        let c = network("@@||x.com^$script,third-party");
        assert_ne!(a.key(), c.key());
    }
}

#![forbid(unsafe_code)]

//! A tag scanner for `stack.xml`, which is untrusted input.
//!
//! Not an XML parser: it yields the start, end and empty tags with their
//! attributes and skips text, comments, processing instructions, CDATA and
//! DOCTYPE (so there are no entities to expand and nothing to blow up). Only
//! the five predefined entities and numeric references are decoded in
//! attribute values. A tag count caps the work.

/// What kind of tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `<name ...>`
    Open,
    /// `</name>`
    Close,
    /// `<name ... />`
    Empty,
}

/// One tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// Open, close or empty.
    pub kind: Kind,
    /// The element name.
    pub name: String,
    /// Attributes in document order, values decoded.
    pub attrs: Vec<(String, String)>,
}

impl Tag {
    /// The value of attribute `name`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Every tag of `text`, or an error for markup that is cut short or has more
/// than `max_tags` tags.
pub fn tags(text: &str, max_tags: usize) -> Result<Vec<Tag>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(lt) = find(b, i, b"<") {
        i = lt;
        if b[i..].starts_with(b"<!--") {
            i = skip_past(b, i + 4, b"-->")?;
        } else if b[i..].starts_with(b"<![CDATA[") {
            i = skip_past(b, i + 9, b"]]>")?;
        } else if b[i..].starts_with(b"<?") {
            i = skip_past(b, i + 2, b"?>")?;
        } else if b[i..].starts_with(b"<!") {
            i = skip_doctype(b, i + 2)?;
        } else {
            if out.len() >= max_tags {
                return Err("stack.xml has too many elements".to_string());
            }
            let (tag, next) = parse_tag(text, i + 1)?;
            out.push(tag);
            i = next;
        }
    }
    Ok(out)
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn skip_past(b: &[u8], from: usize, end: &[u8]) -> Result<usize, String> {
    find(b, from, end)
        .map(|p| p + end.len())
        .ok_or_else(|| "stack.xml is cut short".to_string())
}

/// Past a `<!DOCTYPE ...>` including a bracketed internal subset.
fn skip_doctype(b: &[u8], from: usize) -> Result<usize, String> {
    let mut depth = 0u32;
    for (n, &c) in b.iter().enumerate().skip(from) {
        match c {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return Ok(n + 1),
            _ => {}
        }
    }
    Err("stack.xml is cut short".to_string())
}

fn parse_tag(text: &str, from: usize) -> Result<(Tag, usize), String> {
    let b = text.as_bytes();
    let bad = || "malformed element in stack.xml".to_string();
    let mut i = from;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_end = scan(b, i, |c| c.is_ascii_whitespace() || c == b'/' || c == b'>');
    let name = text[i..name_end].to_string();
    if name.is_empty() {
        return Err(bad());
    }
    i = name_end;
    let mut attrs = Vec::new();
    loop {
        i = scan(b, i, |c| !c.is_ascii_whitespace());
        match b.get(i).ok_or_else(bad)? {
            b'>' => {
                let kind = if closing { Kind::Close } else { Kind::Open };
                return Ok((Tag { kind, name, attrs }, i + 1));
            }
            b'/' => {
                if b.get(i + 1) != Some(&b'>') {
                    return Err(bad());
                }
                return Ok((
                    Tag {
                        kind: if closing { Kind::Close } else { Kind::Empty },
                        name,
                        attrs,
                    },
                    i + 2,
                ));
            }
            _ => {}
        }
        let key_end = scan(b, i, |c| {
            c.is_ascii_whitespace() || c == b'=' || c == b'>' || c == b'/'
        });
        let key = text[i..key_end].to_string();
        i = scan(b, key_end, |c| !c.is_ascii_whitespace());
        if key.is_empty() || b.get(i) != Some(&b'=') {
            return Err(bad());
        }
        i = scan(b, i + 1, |c| !c.is_ascii_whitespace());
        let quote = *b.get(i).ok_or_else(bad)?;
        if quote != b'"' && quote != b'\'' {
            return Err(bad());
        }
        let value_end = find(b, i + 1, &[quote]).ok_or_else(bad)?;
        attrs.push((key, decode(&text[i + 1..value_end])));
        i = value_end + 1;
    }
}

/// The first index at or after `from` where `stop` holds (the length if none).
fn scan(b: &[u8], from: usize, stop: impl Fn(u8) -> bool) -> usize {
    (from..b.len()).find(|&n| stop(b[n])).unwrap_or(b.len())
}

/// Decodes `&lt; &gt; &amp; &quot; &apos; &#N; &#xN;`; anything else is kept.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escapes `s` for use inside a double-quoted attribute value.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not allowed in XML 1.0 at all.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_attributes_are_scanned() {
        let xml = r#"<?xml version="1.0"?>
            <!DOCTYPE image [ <!ENTITY x "y"> ]>
            <!-- a comment <layer name="no"/> -->
            <image w="3" h='2'><stack opacity="1.0"><layer name="a &amp; b &#65;&#x42;" src="data/0.png"/></stack>text<![CDATA[<layer/>]]></image>"#;
        let t = tags(xml, 100).unwrap();
        let names: Vec<_> = t.iter().map(|t| (t.kind, t.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (Kind::Open, "image"),
                (Kind::Open, "stack"),
                (Kind::Empty, "layer"),
                (Kind::Close, "stack"),
                (Kind::Close, "image"),
            ]
        );
        assert_eq!(t[0].attr("h"), Some("2"));
        assert_eq!(t[2].attr("name"), Some("a & b AB"));
        assert_eq!(t[2].attr("missing"), None);
    }

    #[test]
    fn malformed_markup_is_an_error() {
        for bad in [
            "<a",
            "<a b>",
            "<a b=c/>",
            "<a b=\"x>",
            "<!-- never ends",
            "<>",
            "<a / >",
            "<![CDATA[ no end",
        ] {
            assert!(tags(bad, 10).is_err(), "{bad:?}");
        }
        assert!(tags("<a/><a/><a/>", 2).is_err(), "tag cap");
        assert!(tags("", 2).unwrap().is_empty());
        assert!(tags("just text", 2).unwrap().is_empty());
    }

    #[test]
    fn entities_that_are_not_ours_stay_text() {
        let t = tags(r#"<a v="&bogus; &#xZZ; &#1114112; & &lt;"/>"#, 5).unwrap();
        assert_eq!(t[0].attr("v"), Some("&bogus; &#xZZ; &#1114112; & <"));
    }

    #[test]
    fn escape_round_trips_through_decode() {
        let s = "a<b>&\"c'd é";
        let t = tags(&format!("<a v=\"{}\"/>", escape(s)), 5).unwrap();
        assert_eq!(t[0].attr("v"), Some(s));
        assert_eq!(escape("x\u{1}y"), "xy");
    }
}

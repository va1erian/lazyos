//! `mailto:` links: Mail is the app `mimed` starts for one
//! (`x-scheme-handler/mailto`), with the link as its argument, and opens a
//! new message to its address (RFC 6068: `mailto:to?subject=...&body=...`).

/// What a `mailto:` link fills in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mailto {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// The `mailto:` link among the program's arguments, if there is one.
pub fn from_args(args: impl Iterator<Item = String>) -> Option<Mailto> {
    args.skip(1).find_map(|arg| parse(&arg))
}

/// Parses a `mailto:` URL (any case); `None` for anything else.
pub fn parse(url: &str) -> Option<Mailto> {
    let rest = url
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("mailto:"))
        .map(|_| &url[7..])?;
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut link = Mailto {
        to: decode(to),
        ..Mailto::default()
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(value);
        match key.to_ascii_lowercase().as_str() {
            "to" if !value.is_empty() => {
                if !link.to.is_empty() {
                    link.to.push_str(", ");
                }
                link.to.push_str(&value);
            }
            "subject" => link.subject = value,
            "body" => link.body = value,
            _ => {}
        }
    }
    Some(link)
}

/// Percent-decoding; a broken escape is kept as written, and the result is
/// UTF-8 with anything invalid replaced.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied(),
            bytes.get(i + 2).copied(),
        ) {
            (b'%', Some(h), Some(l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push((hex(h).unwrap() * 16 + hex(l).unwrap()) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mailto_link_fills_the_message() {
        let link = parse("MAILTO:a@x.test?subject=Hi%20there&body=Line%0Atwo&to=b@x.test").unwrap();
        assert_eq!(link.to, "a@x.test, b@x.test");
        assert_eq!(link.subject, "Hi there");
        assert_eq!(link.body, "Line\ntwo");
        assert_eq!(parse("mailto:").unwrap(), Mailto::default());
        assert_eq!(parse("mailto:a%zz").unwrap().to, "a%zz");
    }

    #[test]
    fn other_arguments_are_not_links() {
        assert_eq!(parse("https://x/"), None);
        assert_eq!(parse("/home/u/a.eml"), None);
        let args = ["mail", "--client", "mailto:a@x.test", "attempt=1"].map(String::from);
        assert_eq!(from_args(args.into_iter()).unwrap().to, "a@x.test");
    }
}

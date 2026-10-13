//! A filter's `$options` as the triggers WebKit has: resource types, first/third
//! party and a top-frame domain condition.

use crate::filter::Opt;
use crate::stats::DropReason;

/// WebKit's resource types in a fixed order; a set of them is a bit mask over this
/// array. `raw` and `document` are aliases WebKit expands (`raw` = `fetch`, `websocket`,
/// `other`, `ping`), so the mask names the parts and `type_names` re-joins them.
const TYPES: [&str; 13] = [
    "top-document",
    "child-document",
    "image",
    "style-sheet",
    "script",
    "font",
    "svg-document",
    "media",
    "popup",
    "ping",
    "fetch",
    "websocket",
    "other",
];

const fn bit(name: &str) -> u16 {
    let mut i = 0;
    while i < TYPES.len() {
        // `const` string comparison, byte by byte.
        let (a, b) = (TYPES[i].as_bytes(), name.as_bytes());
        if a.len() == b.len() {
            let mut j = 0;
            let mut same = true;
            while j < a.len() {
                if a[j] != b[j] {
                    same = false;
                }
                j += 1;
            }
            if same {
                return 1 << i;
            }
        }
        i += 1;
    }
    panic!("unknown resource type")
}

const TOP_DOCUMENT: u16 = bit("top-document");
const RAW: u16 = bit("ping") | bit("fetch") | bit("websocket") | bit("other");
const ALL: u16 = (1 << TYPES.len()) - 1;
/// What a filter without type options applies to: everything but the top-level
/// document and popups, as Adblock Plus does.
const DEFAULT: u16 = ALL & !TOP_DOCUMENT & !bit("popup");

/// An option name that selects resource types.
fn type_bits(name: &str) -> Option<u16> {
    Some(match name {
        "script" => bit("script"),
        "image" => bit("image") | bit("svg-document"),
        "stylesheet" | "css" => bit("style-sheet"),
        "font" => bit("font"),
        "media" => bit("media"),
        "other" => bit("other"),
        "xmlhttprequest" | "xhr" | "fetch" => bit("fetch"),
        "subdocument" | "frame" => bit("child-document"),
        "document" | "doc" => TOP_DOCUMENT,
        "ping" | "beacon" => bit("ping"),
        "websocket" => bit("websocket"),
        "popup" => bit("popup"),
        "all" => ALL,
        _ => return None,
    })
}

/// A filter's options, ready to become a rule.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    /// Resource types in WebKit's names (empty only with `emit_normal` false).
    pub resource_types: Vec<&'static str>,
    /// `"first-party"` or `"third-party"`.
    pub load_type: Option<&'static str>,
    /// `*domain` entries of `if-domain`.
    pub if_domain: Vec<String>,
    /// `*domain` entries of `unless-domain`.
    pub unless_domain: Vec<String>,
    pub case_sensitive: bool,
    pub important: bool,
    /// `@@...$document`: the page itself is not filtered (a rule on the top-level URL).
    pub document_exception: bool,
    /// False when the filter had no other effect than `$document`.
    pub emit_normal: bool,
    /// The filter names a type, a party or a domain: it is not "everything" even with no pattern.
    pub constrained: bool,
}

pub(crate) fn interpret(options: &[Opt<'_>], exception: bool) -> Result<Spec, DropReason> {
    let (mut positive, mut negative) = (0u16, 0u16);
    let mut third: Option<bool> = None;
    let mut domains: Option<(Vec<String>, Vec<String>)> = None;
    let (mut case_sensitive, mut important) = (false, false);

    for opt in options {
        let name = opt.name.as_str();
        if let Some(bits) = type_bits(name) {
            if opt.value.is_some() {
                return Err(DropReason::UnsupportedOption);
            }
            if opt.negated {
                negative |= bits;
            } else {
                positive |= bits;
            }
            continue;
        }
        match name {
            "third-party" | "3p" => set_party(&mut third, !opt.negated)?,
            "first-party" | "1p" => set_party(&mut third, opt.negated)?,
            "domain" | "from" if !opt.negated => {
                if domains.is_some() {
                    return Err(DropReason::UnsupportedOption);
                }
                domains = Some(parse_domains(opt.value)?);
            }
            "match-case" => case_sensitive = !opt.negated,
            "important" if !opt.negated => important = true,
            // A request-level filter does not collapse elements.
            "collapse" => {}
            "badfilter" => {}
            "elemhide" | "ehide" | "generichide" | "ghide" | "specifichide" | "shide" => {
                return Err(DropReason::Cosmetic);
            }
            _ => return Err(DropReason::UnsupportedOption),
        }
    }

    let document_exception = exception && positive & TOP_DOCUMENT != 0;
    if document_exception {
        positive &= !TOP_DOCUMENT;
    }
    let (if_domain, unless_domain) = domains.unwrap_or_default();
    if document_exception && (!if_domain.is_empty() || !unless_domain.is_empty() || third.is_some())
    {
        // A page-wide exception that depends on who is asking has no single trigger.
        return Err(DropReason::UnsupportedOption);
    }

    let base = if positive != 0 { positive } else { DEFAULT };
    let mask = base & !negative;
    let emit_normal = !(document_exception && positive == 0 && negative == 0);
    if emit_normal && mask == 0 {
        return Err(DropReason::UnsupportedOption);
    }

    let constrained = positive != 0
        || negative != 0
        || third.is_some()
        || !if_domain.is_empty()
        || !unless_domain.is_empty();
    Ok(Spec {
        resource_types: if emit_normal {
            type_names(mask)
        } else {
            Vec::new()
        },
        load_type: third.map(|t| if t { "third-party" } else { "first-party" }),
        if_domain,
        unless_domain,
        case_sensitive,
        important,
        document_exception,
        emit_normal,
        constrained,
    })
}

fn set_party(slot: &mut Option<bool>, third: bool) -> Result<(), DropReason> {
    match slot {
        Some(existing) if *existing != third => Err(DropReason::UnsupportedOption),
        _ => {
            *slot = Some(third);
            Ok(())
        }
    }
}

/// `a.com|b.com` or `~a.com|~b.com`. `if-domain` and `unless-domain` cannot share a
/// trigger, and a mix is dropped rather than approximated.
fn parse_domains(value: Option<&str>) -> Result<(Vec<String>, Vec<String>), DropReason> {
    let value = value.ok_or(DropReason::UnsupportedOption)?;
    let (mut included, mut excluded) = (Vec::new(), Vec::new());
    for entry in value.split('|') {
        let (entry, negated) = match entry.strip_prefix('~') {
            Some(rest) => (rest, true),
            None => (entry, false),
        };
        let entry = entry.to_ascii_lowercase();
        if !valid_domain(&entry) {
            return Err(DropReason::UnsupportedOption);
        }
        // A leading `*` makes WebKit match the domain and its subdomains, as `domain=` does.
        let entry = format!("*{entry}");
        if negated {
            excluded.push(entry);
        } else {
            included.push(entry);
        }
    }
    if !included.is_empty() && !excluded.is_empty() {
        return Err(DropReason::MixedDomains);
    }
    Ok((included, excluded))
}

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && !d.starts_with('.')
        && !d.ends_with('.')
        && !d.starts_with('-')
        && d.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// A mask in WebKit's names, joining the four request-like types into `raw`.
fn type_names(mask: u16) -> Vec<&'static str> {
    let raw = mask & RAW == RAW;
    let mut names: Vec<&'static str> = TYPES
        .iter()
        .enumerate()
        .filter(|(i, name)| {
            mask & (1 << i) != 0
                && !(raw && matches!(**name, "ping" | "fetch" | "websocket" | "other"))
        })
        .map(|(_, name)| *name)
        .collect();
    if raw {
        names.push("raw");
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{parse_line, Line};

    fn spec(line: &str) -> Result<Spec, DropReason> {
        match parse_line(line) {
            Line::Network(f) => interpret(&f.options, f.exception),
            _ => panic!("not a network filter"),
        }
    }

    #[test]
    fn no_options_means_everything_but_documents_and_popups() {
        let s = spec("||x.com^").unwrap();
        assert_eq!(
            s.resource_types,
            [
                "child-document",
                "image",
                "style-sheet",
                "script",
                "font",
                "svg-document",
                "media",
                "raw"
            ]
        );
        assert_eq!(s.load_type, None);
    }

    #[test]
    fn types_select_and_negate() {
        assert_eq!(
            spec("x$script,image").unwrap().resource_types,
            ["image", "script", "svg-document"]
        );
        let s = spec("x$~script,~image").unwrap();
        assert!(!s.resource_types.contains(&"script") && !s.resource_types.contains(&"image"));
        assert!(s.resource_types.contains(&"font"));
        assert_eq!(spec("x$document").unwrap().resource_types, ["top-document"]);
        assert_eq!(spec("x$xmlhttprequest").unwrap().resource_types, ["fetch"]);
        assert_eq!(
            spec("x$subdocument").unwrap().resource_types,
            ["child-document"]
        );
        assert_eq!(spec("x$script,~script"), Err(DropReason::UnsupportedOption));
    }

    #[test]
    fn party_and_domains() {
        assert_eq!(
            spec("x$third-party").unwrap().load_type,
            Some("third-party")
        );
        assert_eq!(
            spec("x$~third-party").unwrap().load_type,
            Some("first-party")
        );
        assert_eq!(spec("x$1p").unwrap().load_type, Some("first-party"));
        assert_eq!(
            spec("x$third-party,first-party"),
            Err(DropReason::UnsupportedOption)
        );
        let s = spec("x$domain=A.com|b.org").unwrap();
        assert_eq!(s.if_domain, ["*a.com", "*b.org"]);
        let s = spec("x$domain=~a.com").unwrap();
        assert_eq!(s.unless_domain, ["*a.com"]);
        assert_eq!(spec("x$domain=a.com|~b.com"), Err(DropReason::MixedDomains));
        assert_eq!(
            spec("x$domain=example.*"),
            Err(DropReason::UnsupportedOption)
        );
        assert_eq!(spec("x$domain=/re/"), Err(DropReason::UnsupportedOption));
    }

    #[test]
    fn what_webkit_cannot_express_is_refused() {
        for line in [
            "x$redirect=noopjs",
            "x$csp=script-src 'none'",
            "x$removeparam=a",
            "x$denyallow=a.com",
            "x$to=a.com",
            "x$bogus",
        ] {
            assert_eq!(spec(line), Err(DropReason::UnsupportedOption), "{line}");
        }
        for line in ["@@x$elemhide", "@@x$generichide"] {
            assert_eq!(spec(line), Err(DropReason::Cosmetic), "{line}");
        }
    }

    #[test]
    fn a_document_exception_is_a_page_wide_whitelist() {
        let s = spec("@@||x.com^$document").unwrap();
        assert!(s.document_exception);
        assert!(!s.emit_normal);
        let s = spec("@@||x.com^$document,script").unwrap();
        assert!(s.document_exception && s.emit_normal);
        assert_eq!(s.resource_types, ["script"]);
        assert_eq!(
            spec("@@||x.com^$document,domain=a.com"),
            Err(DropReason::UnsupportedOption)
        );
        // On a block, `$document` blocks the top-level load itself.
        assert!(!spec("||x.com^$document").unwrap().document_exception);
    }

    #[test]
    fn match_case_and_important() {
        let s = spec("x$match-case,important").unwrap();
        assert!(s.case_sensitive && s.important);
    }
}

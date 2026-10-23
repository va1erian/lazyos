//! One content rule as JSON, and the rules laid out as WebKit lists.

use crate::RuleList;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Block,
    /// Cancels earlier rules *of the same list* that match.
    IgnorePrevious,
    /// `css-display-none`: hides the elements `Rule::selector` matches.
    HideElements,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub action: Action,
    pub url_filter: String,
    pub case_sensitive: bool,
    pub resource_types: Vec<&'static str>,
    pub load_type: Option<&'static str>,
    pub if_domain: Vec<String>,
    pub unless_domain: Vec<String>,
    /// Regexes on the top-level URL (a page-wide exception).
    pub if_top_url: Vec<String>,
    /// What `HideElements` hides (a CSS selector list); `None` for every other action.
    pub selector: Option<String>,
}

impl Rule {
    /// `{"trigger":{...},"action":{...}}`.
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(96 + self.url_filter.len());
        s.push_str("{\"trigger\":{\"url-filter\":");
        string(&mut s, &self.url_filter);
        if self.case_sensitive {
            s.push_str(",\"url-filter-is-case-sensitive\":true");
            if !self.if_top_url.is_empty() {
                s.push_str(",\"top-url-filter-is-case-sensitive\":true");
            }
        }
        if !self.resource_types.is_empty() {
            list(&mut s, "resource-type", self.resource_types.iter().copied());
        }
        if let Some(load_type) = self.load_type {
            list(&mut s, "load-type", [load_type]);
        }
        if !self.if_domain.is_empty() {
            list(
                &mut s,
                "if-domain",
                self.if_domain.iter().map(String::as_str),
            );
        }
        if !self.unless_domain.is_empty() {
            list(
                &mut s,
                "unless-domain",
                self.unless_domain.iter().map(String::as_str),
            );
        }
        if !self.if_top_url.is_empty() {
            list(
                &mut s,
                "if-top-url",
                self.if_top_url.iter().map(String::as_str),
            );
        }
        s.push_str("},\"action\":{\"type\":\"");
        s.push_str(match self.action {
            Action::Block => "block",
            Action::IgnorePrevious => "ignore-previous-rules",
            Action::HideElements => "css-display-none",
        });
        s.push('"');
        if let Some(selector) = &self.selector {
            s.push_str(",\"selector\":");
            string(&mut s, selector);
        }
        s.push_str("}}");
        s
    }
}

fn list<'a>(s: &mut String, key: &str, items: impl IntoIterator<Item = &'a str>) {
    s.push_str(",\"");
    s.push_str(key);
    s.push_str("\":[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        string(s, item);
    }
    s.push(']');
}

fn string(s: &mut String, value: &str) {
    s.push('"');
    for c in value.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

/// Lays serialized rules out as lists of at most `max` rules. An exception cancels
/// earlier rules of its own list only, so every list carries all the exceptions after
/// its share of the blocks, and `$important` blocks come last so none is cancelled.
///
/// Exceptions and important blocks are capped at half and a quarter of a list, so a
/// list always holds blocks; what did not fit is counted in `over`.
pub(crate) struct Assembled {
    pub lists: Vec<RuleList>,
    pub exceptions: usize,
    pub important: usize,
    pub over: usize,
}

pub(crate) fn assemble(
    blocks: Vec<String>,
    mut exceptions: Vec<String>,
    mut important: Vec<String>,
    max: usize,
) -> Assembled {
    let max = max.max(4);
    let mut over = 0;
    if exceptions.len() > max / 2 {
        over += exceptions.len() - max / 2;
        exceptions.truncate(max / 2);
    }
    if important.len() > max / 4 {
        over += important.len() - max / 4;
        important.truncate(max / 4);
    }
    let (kept_exceptions, kept_important) = (exceptions.len(), important.len());
    if blocks.is_empty() {
        return Assembled {
            lists: Vec::new(),
            exceptions: kept_exceptions,
            important: kept_important,
            over,
        };
    }
    let share = max - exceptions.len() - important.len();
    let lists = blocks
        .chunks(share)
        .map(|chunk| {
            let rules = chunk.len() + exceptions.len() + important.len();
            let mut json = String::new();
            json.push_str("[\n");
            let mut first = true;
            for rule in chunk.iter().chain(&exceptions).chain(&important) {
                if !first {
                    json.push_str(",\n");
                }
                first = false;
                json.push_str(rule);
            }
            json.push_str("\n]\n");
            RuleList { json, rules }
        })
        .collect();
    Assembled {
        lists,
        exceptions: kept_exceptions,
        important: kept_important,
        over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Rule {
        Rule {
            action: Action::Block,
            url_filter: r"^[^:]+://+([^:/?#]+\.)?x\.com".into(),
            case_sensitive: false,
            resource_types: vec!["script", "image"],
            load_type: Some("third-party"),
            if_domain: vec!["*a.com".into()],
            unless_domain: Vec::new(),
            if_top_url: Vec::new(),
            selector: None,
        }
    }

    #[test]
    fn a_hiding_rule_carries_its_selector() {
        let mut r = rule();
        r.action = Action::HideElements;
        r.selector = Some(".ad, #x[a=\"b\"]".into());
        assert!(r
            .to_json()
            .ends_with(r#""action":{"type":"css-display-none","selector":".ad, #x[a=\"b\"]"}}"#));
    }

    #[test]
    fn a_rule_serializes_in_a_fixed_key_order() {
        assert_eq!(
            rule().to_json(),
            r#"{"trigger":{"url-filter":"^[^:]+://+([^:/?#]+\\.)?x\\.com","resource-type":["script","image"],"load-type":["third-party"],"if-domain":["*a.com"]},"action":{"type":"block"}}"#
        );
    }

    #[test]
    fn strings_are_escaped() {
        let mut r = rule();
        r.url_filter = "a\"b\\c\u{1}".into();
        assert!(r.to_json().contains(r#""url-filter":"a\"b\\c\u0001""#));
    }

    #[test]
    fn lists_split_and_repeat_the_exceptions() {
        let blocks: Vec<String> = (0..10).map(|i| format!("{{\"b\":{i}}}")).collect();
        let exceptions = vec!["{\"e\":0}".to_owned(), "{\"e\":1}".to_owned()];
        let important = vec!["{\"i\":0}".to_owned()];
        let Assembled { lists, over, .. } = assemble(blocks, exceptions, important, 8);
        assert_eq!(over, 0);
        // 8 - 2 - 1 = 5 blocks per list: two lists.
        assert_eq!(lists.len(), 2);
        assert!(lists.iter().all(|l| l.rules <= 8));
        for l in &lists {
            assert!(
                l.json.contains("\"e\":0")
                    && l.json.contains("\"e\":1")
                    && l.json.contains("\"i\":0")
            );
            // Exceptions come after the blocks and before the important ones.
            let e = l.json.find("\"e\":0").unwrap();
            assert!(l.json.find("\"b\"").unwrap() < e && e < l.json.find("\"i\":0").unwrap());
        }
    }

    #[test]
    fn no_blocks_means_no_list() {
        let out = assemble(Vec::new(), vec!["{}".into()], Vec::new(), 100);
        assert!(out.lists.is_empty());
    }

    #[test]
    fn exceptions_are_capped_so_blocks_still_fit() {
        let exceptions: Vec<String> = (0..100).map(|i| format!("{{\"e\":{i}}}")).collect();
        let Assembled { lists, over, .. } =
            assemble(vec!["{\"b\":0}".into()], exceptions, Vec::new(), 20);
        assert_eq!(over, 90);
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].rules, 11);
    }
}

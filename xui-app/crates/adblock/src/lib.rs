//! Adblock Plus filter lists as WebKit content-rule lists (issue #845, LazyWeb+).
//!
//! WebKit blocks requests with *content extensions*: JSON rule lists in Safari's
//! content-blocker format, compiled by the UI process to a bytecode DFA. This crate
//! turns EasyList-style text into that JSON, deterministically and without
//! approximating: a filter WebKit cannot express exactly is dropped and counted
//! ([`DropReason`]), never widened, because a rule that blocks more than the list meant
//! breaks pages.
//!
//! What it emits, and why (all of it from WebKit 2.54's `ContentExtensionParser.cpp`,
//! `URLFilterParser.cpp` and `ResourceLoadInfo.cpp`):
//!
//! * `block` and `ignore-previous-rules` only; element hiding is not converted (v1).
//! * One pattern WebKit refuses fails the *whole* list (`JSONInvalidRegex`), so
//!   [`pattern`] emits only the URL-filter subset WebKit parses, and the tests parse
//!   every output with an independent parser of that subset.
//! * A list holds 150,000 rules; a longer one is split, and each part repeats the
//!   exceptions because `ignore-previous-rules` only reaches back inside its own list.
//! * A trailing `^` matches a separator character, not the end of the URL: such a
//!   filter can only block less than Adblock Plus would.
//!
//! The output is a pure function of the input: the same text gives byte-identical
//! JSON, so a hash of the source names a compiled list in WebKit's store.

mod filter;
mod options;
mod pattern;
mod rules;
mod stats;

use std::collections::HashSet;

use filter::{parse_line, Filter, Line};
use rules::{assemble, Action, Rule};
pub use stats::{DropReason, Stats};

/// WebKit's per-list rule limit (`maxRuleCount` in `ContentExtensionParser.cpp`).
pub const WEBKIT_MAX_RULES: usize = 150_000;

/// Conversion settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Rules per list; at most [`WEBKIT_MAX_RULES`] is accepted by WebKit.
    pub max_rules_per_list: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_rules_per_list: WEBKIT_MAX_RULES,
        }
    }
}

/// One JSON rule list, ready for `WKUserContentExtensionStoreCompile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleList {
    pub json: String,
    /// How many rules it holds.
    pub rules: usize,
}

/// The result of [`convert`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    /// Empty when the source holds no network filter WebKit can express.
    pub lists: Vec<RuleList>,
    pub stats: Stats,
}

/// What one filter becomes.
struct Built {
    key: String,
    rules: Vec<Rule>,
    exception: bool,
    important: bool,
}

/// Converts the text of a filter list.
pub fn convert(source: &str, options: &Options) -> Converted {
    let mut stats = Stats::default();
    let mut built: Vec<Built> = Vec::new();
    let mut badfilters: HashSet<String> = HashSet::new();

    for raw in source.lines() {
        stats.lines += 1;
        match parse_line(raw) {
            Line::Skip => stats.comments += 1,
            Line::Cosmetic => stats.drop_filter(DropReason::Cosmetic),
            Line::Network(f) => {
                if f.options.iter().any(|o| o.name == "badfilter") {
                    badfilters.insert(f.key());
                    continue;
                }
                match build(&f) {
                    Ok(b) => built.push(b),
                    Err(reason) => {
                        // An element-hiding exception cancels no block: not what this counts.
                        if f.exception && reason != DropReason::Cosmetic {
                            stats.dropped_exceptions += 1;
                        }
                        stats.drop_filter(reason);
                    }
                }
            }
        }
    }

    // A duplicate is the same rule in the same place: `||x^` and `||x^$important` are not.
    let mut seen: HashSet<(bool, bool, String)> = HashSet::new();
    let (mut blocks, mut exceptions, mut important) = (Vec::new(), Vec::new(), Vec::new());
    for b in built {
        if badfilters.contains(&b.key) {
            stats.badfiltered += 1;
            continue;
        }
        for rule in b.rules {
            let json = rule.to_json();
            if !seen.insert((b.exception, b.important, json.clone())) {
                stats.duplicates += 1;
                continue;
            }
            if b.exception {
                exceptions.push(json);
            } else if b.important {
                important.push(json);
            } else {
                blocks.push(json);
            }
        }
    }

    stats.blocks = blocks.len();
    let out = assemble(blocks, exceptions, important, options.max_rules_per_list);
    for _ in 0..out.over {
        stats.drop_filter(DropReason::OverLimit);
    }
    stats.exceptions = out.exceptions;
    stats.important = out.important;
    Converted {
        lists: out.lists,
        stats,
    }
}

fn build(f: &Filter<'_>) -> Result<Built, DropReason> {
    let spec = options::interpret(&f.options, f.exception)?;
    let url_filter = pattern::url_filter(f.pattern)?;
    // A pattern that is only anchors and wildcards matches every address: with no type, party or
    // domain to narrow it, that is a list gone wrong (a truncated line), not a rule.
    if !spec.constrained && f.pattern.bytes().all(|b| matches!(b, b'|' | b'*' | b'^')) {
        return Err(DropReason::UnsupportedPattern);
    }
    let mut rules = Vec::new();

    if spec.emit_normal {
        rules.push(Rule {
            action: if f.exception {
                Action::IgnorePrevious
            } else {
                Action::Block
            },
            url_filter: url_filter.clone(),
            case_sensitive: spec.case_sensitive,
            resource_types: spec.resource_types.clone(),
            load_type: spec.load_type,
            if_domain: spec.if_domain.clone(),
            unless_domain: spec.unless_domain.clone(),
            if_top_url: Vec::new(),
        });
    }
    if spec.document_exception {
        // The page itself is not filtered: every request, while the top-level URL
        // is the one the pattern names.
        rules.push(Rule {
            action: Action::IgnorePrevious,
            url_filter: ".*".to_owned(),
            case_sensitive: spec.case_sensitive,
            resource_types: Vec::new(),
            load_type: None,
            if_domain: Vec::new(),
            unless_domain: Vec::new(),
            if_top_url: vec![url_filter],
        });
    }
    Ok(Built {
        key: f.key(),
        rules,
        exception: f.exception,
        important: spec.important && !f.exception,
    })
}

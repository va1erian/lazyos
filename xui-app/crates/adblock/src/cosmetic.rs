//! Element hiding: `domains##selector` becomes a `css-display-none` rule and
//! `domains#@#selector` takes the selector back off the domains it names.
//!
//! WebKit refuses a whole list for one selector it cannot parse, and a selector whose meaning
//! differs between Adblock Plus and WebKit would hide the wrong boxes, so only plain CSS is
//! taken: type, `#id`, `.class` and `[attribute]` tests joined by the four combinators. Anything
//! with a pseudo-class, an escape or a comma is dropped and counted, never approximated.

use std::collections::{BTreeMap, BTreeSet};

use crate::filter::{Filter, Hide};
use crate::options::{parse_domains, valid_domain};
use crate::rules::{Action, Rule};
use crate::stats::{DropReason, Stats};

/// The longest selector taken: a hostile or broken list cannot make a page parse a long one.
const MAX_SELECTOR: usize = 1024;

/// Selectors that share a trigger go in one rule, so a list does not spend a rule on each.
const SELECTORS_PER_RULE: usize = 64;

/// One `##` or `#@#` line with its selector checked and its domains read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hiding {
    selector: String,
    exception: bool,
    /// `*domain` entries (WebKit's spelling for the domain and its subdomains).
    include: BTreeSet<String>,
    exclude: BTreeSet<String>,
}

impl Hiding {
    /// `Err` with why the line cannot become a rule.
    pub fn new(hide: &Hide<'_>) -> Result<Hiding, DropReason> {
        if !valid_selector(hide.selector) {
            return Err(DropReason::Cosmetic);
        }
        let (include, exclude) = if hide.domains.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            // A wildcard domain (`site.*`) has no `if-domain` spelling. Leaving it out of a hiding
            // line hides on fewer sites, which is safe; leaving it out of an exception, or an
            // exclusion, would hide on more, so those lines are dropped whole.
            let kept: Vec<&str> = hide
                .domains
                .split(',')
                .filter(|d| hide.exception || d.starts_with('~') || !d.contains('*'))
                .collect();
            if kept.is_empty() {
                return Err(DropReason::Cosmetic);
            }
            parse_domains(Some(&kept.join("|"))).map_err(|reason| match reason {
                DropReason::MixedDomains => reason,
                _ => DropReason::Cosmetic,
            })?
        };
        Ok(Hiding {
            selector: hide.selector.to_owned(),
            exception: hide.exception,
            include: include.into_iter().collect(),
            exclude: exclude.into_iter().collect(),
        })
    }
}

/// `@@||site^$generichide` or `$elemhide`: hiding is off for a site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SiteOff {
    /// `*site`: the site and its subdomains.
    site: String,
    /// `$elemhide`: no hiding at all; `$generichide`: only the rules every site gets.
    all: bool,
}

impl SiteOff {
    /// The `@@||site^$generichide` (or `$elemhide`) form, and nothing else: a path, a port or a
    /// further option has no `if-domain` spelling, and stays dropped.
    pub fn new(f: &Filter<'_>) -> Option<SiteOff> {
        let [option] = f.options.as_slice() else {
            return None;
        };
        let all = match option.name.as_str() {
            "elemhide" | "ehide" => true,
            "generichide" | "ghide" => false,
            _ => return None,
        };
        let site = f.pattern.strip_prefix("||")?.strip_suffix('^')?;
        let site = site.to_ascii_lowercase();
        (f.exception && !option.negated && option.value.is_none() && valid_domain(&site)).then(
            || SiteOff {
                site: format!("*{site}"),
                all,
            },
        )
    }

    /// The exception this is, for a hiding rule of any selector.
    fn as_exception(&self) -> Hiding {
        Hiding {
            selector: String::new(),
            exception: true,
            include: BTreeSet::from([self.site.clone()]),
            exclude: BTreeSet::new(),
        }
    }
}

/// The hiding rules of a list's `##` and `#@#` lines, in a fixed order. `stats.hides` counts the
/// selectors kept. An exception that cannot be applied exactly removes the selector instead
/// (hiding less); one that names a selector no line hides has nothing to cancel and is ignored.
pub(crate) fn rules(lines: Vec<Hiding>, offs: &[SiteOff], stats: &mut Stats) -> Vec<Rule> {
    let (exceptions, hides): (Vec<Hiding>, Vec<Hiding>) =
        lines.into_iter().partition(|l| l.exception);
    let mut taken: BTreeMap<&str, Vec<&Hiding>> = BTreeMap::new();
    for e in &exceptions {
        taken.entry(e.selector.as_str()).or_default().push(e);
    }
    let mut groups: Groups = BTreeMap::new();
    for original in &hides {
        let mut hide = original.clone();
        let list = taken
            .get(original.selector.as_str())
            .map_or(&[][..], |l| &l[..]);
        if !list.iter().all(|e| except(&mut hide, e)) {
            continue;
        }
        let key = (hide.include, hide.exclude);
        groups.entry(key).or_default().insert(hide.selector);
    }
    // Site-level exceptions act on a group, not on each selector: few groups, many selectors.
    let groups = spare_sites(groups, offs);

    let mut out = Vec::new();
    for ((include, exclude), selectors) in groups {
        stats.hides += selectors.len();
        let selectors: Vec<String> = selectors.into_iter().collect();
        for chunk in selectors.chunks(SELECTORS_PER_RULE) {
            out.push(Rule {
                action: Action::HideElements,
                url_filter: ".*".to_owned(),
                case_sensitive: false,
                resource_types: Vec::new(),
                load_type: None,
                if_domain: include.iter().cloned().collect(),
                unless_domain: exclude.iter().cloned().collect(),
                if_top_url: Vec::new(),
                selector: Some(chunk.join(", ")),
            });
        }
    }
    out
}

/// Selectors by the domains they hide on: `(include, exclude)`.
type Groups = BTreeMap<(BTreeSet<String>, BTreeSet<String>), BTreeSet<String>>;

/// `groups` with every site in `offs` spared: from the rules every site gets (`$generichide`), and
/// from the listed ones too (`$elemhide`). Groups that become alike are merged.
fn spare_sites(groups: Groups, offs: &[SiteOff]) -> Groups {
    let mut out: Groups = BTreeMap::new();
    for ((include, exclude), selectors) in groups {
        let generic = include.is_empty();
        let mut spared = Hiding {
            selector: String::new(),
            exception: false,
            include,
            exclude,
        };
        let mut alive = true;
        for off in offs.iter().filter(|o| o.all || generic) {
            alive = alive && except(&mut spared, &off.as_exception());
        }
        if alive {
            out.entry((spared.include, spared.exclude))
                .or_default()
                .extend(selectors);
        }
    }
    out
}

/// Applies exception `e` to `hide` (same selector). False when nothing of the rule is left.
fn except(hide: &mut Hiding, e: &Hiding) -> bool {
    if e.include.is_empty() {
        // `#@#sel` everywhere, or everywhere but some domains: the selector is not hidden.
        return false;
    }
    if hide.include.is_empty() {
        // Hidden everywhere (or everywhere but some domains), but not on these.
        hide.exclude.extend(e.include.iter().cloned());
        return true;
    }
    // Hidden on listed domains only: drop each one the exception reaches. A listed domain that
    // contains an excepted one (`a.com`, and `#@#` on `b.a.com`) cannot be carved, so it goes too.
    hide.include
        .retain(|i| !e.include.iter().any(|x| covers(x, i) || covers(i, x)));
    !hide.include.is_empty()
}

/// Whether `outer` (`*a.com`) is `inner` (`*b.a.com`) or a parent of it.
fn covers(outer: &str, inner: &str) -> bool {
    let (outer, inner) = (&outer[1..], &inner[1..]);
    inner == outer
        || inner
            .strip_suffix(outer)
            .is_some_and(|head| head.ends_with('.'))
}

/// Whether `s` is a selector of the plain-CSS subset described in the module docs. It must
/// test an id, a class or an attribute somewhere (a bare `div` hides far more than a list
/// means) and must not end on a bare `*`, `html` or `body`.
pub(crate) fn valid_selector(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_SELECTOR || !s.is_ascii() {
        return false;
    }
    let b = s.as_bytes();
    let (mut i, mut marked) = (0, false);
    loop {
        let Some(c) = compound(b, i) else {
            return false;
        };
        marked |= c.marked;
        i = skip_space(b, c.end);
        match b.get(i) {
            None => return marked && !c.root,
            Some(b'>' | b'+' | b'~') => i = skip_space(b, i + 1),
            // Whitespace was a descendant combinator only when another compound follows.
            Some(_) if i == c.end => return false,
            Some(_) => {}
        }
    }
}

fn skip_space(b: &[u8], mut i: usize) -> usize {
    while b.get(i) == Some(&b' ') {
        i += 1;
    }
    i
}

struct Compound {
    end: usize,
    /// It tests an id, a class or an attribute.
    marked: bool,
    /// It is only `*`, `html` or `body`.
    root: bool,
}

/// One compound selector at `i`; `None` when it is not valid.
fn compound(b: &[u8], mut i: usize) -> Option<Compound> {
    let start = i;
    let mut root = false;
    if b.get(i) == Some(&b'*') {
        i += 1;
        root = true;
    } else if let Some(end) = ident(b, i) {
        let tag = &b[i..end];
        root = tag.eq_ignore_ascii_case(b"html") || tag.eq_ignore_ascii_case(b"body");
        i = end;
    }
    let typed = i > start;
    let mut marked = false;
    while let Some(c) = b.get(i) {
        i = match c {
            b'#' | b'.' => ident(b, i + 1)?,
            b'[' => attribute(b, i + 1)?,
            _ => break,
        };
        marked = true;
    }
    (typed || marked).then_some(Compound {
        end: i,
        marked,
        root: root && !marked,
    })
}

/// An identifier at `i` (a letter or `_`, after an optional `-`, then letters, digits, `_` and
/// `-`): its end. Escapes and non-ASCII are not taken.
fn ident(b: &[u8], mut i: usize) -> Option<usize> {
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(c) if c.is_ascii_alphabetic() || *c == b'_' => i += 1,
        _ => return None,
    }
    while b
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        i += 1;
    }
    Some(i)
}

/// The rest of `[name]`, `[name=value]` (or `^=`, `$=`, `*=`, `~=`, `|=`) after its `[`: the index
/// after the `]`. A value is a quoted string without escapes or an identifier.
fn attribute(b: &[u8], i: usize) -> Option<usize> {
    let mut i = ident(b, i)?;
    if b.get(i) == Some(&b']') {
        return Some(i + 1);
    }
    i += match (b.get(i)?, b.get(i + 1)) {
        (b'=', _) => 1,
        (b'^' | b'$' | b'*' | b'~' | b'|', Some(b'=')) => 2,
        _ => return None,
    };
    i = match b.get(i)? {
        q @ (b'"' | b'\'') => {
            let len = b[i + 1..].iter().position(|c| c == q)?;
            if b[i + 1..i + 1 + len]
                .iter()
                .any(|c| *c == b'\\' || *c < b' ')
            {
                return None;
            }
            i + len + 2
        }
        _ => ident(b, i)?,
    };
    (b.get(i) == Some(&b']')).then_some(i + 1)
}

#[cfg(test)]
mod tests;

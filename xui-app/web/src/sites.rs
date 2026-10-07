//! Per-site preferences: how LazyWeb asks a few sites for pages NetSurf
//! renders well.
//!
//! Wikipedia and its sister projects serve the Vector 2022 skin by default,
//! whose layout is a CSS grid with custom properties, neither of which
//! NetSurf has: the page collapses to one column with every menu showing. The
//! 2010 Vector skin, which the wikis still serve with `useskin=vector`, lays
//! out with floats and absolute positions and was built for browsers without
//! JavaScript, so LazyWeb asks for that one (issue #632). The rewrite happens
//! at the fetch: NetSurf, the address bar and history keep the URL the page
//! linked to, and every link followed on the wiki is rewritten again.

use std::borrow::Cow;

/// Domains (and their subdomains) that run MediaWiki with the Vector skins.
const WIKIMEDIA: &[&str] = &[
    "wikipedia.org",
    "wiktionary.org",
    "wikibooks.org",
    "wikiquote.org",
    "wikisource.org",
    "wikiversity.org",
    "wikivoyage.org",
    "wikinews.org",
    "wikimedia.org",
    "wikidata.org",
    "mediawiki.org",
];

/// The query parameter that picks the 2010 Vector skin.
const SKIN: &str = "useskin=vector";

/// The URL to fetch for `url`: `url` itself, or a wiki page with the skin
/// LazyWeb prefers. A URL that already picks a skin is left alone.
pub fn fetch_url(url: &str) -> Cow<'_, str> {
    let Some((host, path)) = split(url) else {
        return Cow::Borrowed(url);
    };
    let page = path.starts_with("/wiki/") || path.starts_with("/w/index.php");
    if !page || !is_wikimedia(host) || has_param(path, "useskin") {
        return Cow::Borrowed(url);
    }
    // The skin goes before any fragment, after any query.
    let end = url.find('#').unwrap_or(url.len());
    let joiner = if url[..end].contains('?') { '&' } else { '?' };
    Cow::Owned(format!("{}{joiner}{SKIN}{}", &url[..end], &url[end..]))
}

/// The host (without port or user) and the path with its query and
/// fragment, of an `http:` or `https:` URL.
fn split(url: &str) -> Option<(&str, &str)> {
    let rest = ["https://", "http://"].iter().find_map(|scheme| {
        url.get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| &url[scheme.len()..])
    })?;
    let at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..at];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    Some((host, &rest[at..]))
}

fn is_wikimedia(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    WIKIMEDIA.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|sub| sub.ends_with('.'))
    })
}

/// Whether the query of `path` (before any fragment) has parameter `name`.
fn has_param(path: &str, name: &str) -> bool {
    let path = path.split('#').next().unwrap_or(path);
    let Some((_, query)) = path.split_once('?') else {
        return false;
    };
    query
        .split('&')
        .any(|pair| pair.split('=').next() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_pages_ask_for_the_2010_skin() {
        assert_eq!(
            fetch_url("https://en.wikipedia.org/wiki/Main_Page"),
            "https://en.wikipedia.org/wiki/Main_Page?useskin=vector"
        );
        assert_eq!(
            fetch_url("https://en.wikipedia.org/wiki/1762#Events"),
            "https://en.wikipedia.org/wiki/1762?useskin=vector#Events"
        );
        assert_eq!(
            fetch_url("https://EN.Wikipedia.ORG/w/index.php?title=1762&action=history"),
            "https://EN.Wikipedia.ORG/w/index.php?title=1762&action=history&useskin=vector"
        );
        assert_eq!(
            fetch_url("http://fr.wiktionary.org:80/wiki/chat"),
            "http://fr.wiktionary.org:80/wiki/chat?useskin=vector"
        );
        assert_eq!(
            fetch_url("https://wikipedia.org/wiki/X"),
            "https://wikipedia.org/wiki/X?useskin=vector"
        );
    }

    #[test]
    fn everything_else_is_left_alone() {
        for url in [
            // A skin already chosen, either one.
            "https://en.wikipedia.org/wiki/1762?useskin=vector-2022",
            "https://en.wikipedia.org/w/index.php?useskin=monobook&title=X",
            // Not a page: styles, scripts, pictures, the API.
            "https://en.wikipedia.org/w/load.php?modules=site.styles&skin=vector-2022",
            "https://upload.wikimedia.org/wikipedia/commons/a/a9/Example.jpg",
            "https://en.wikipedia.org/w/api.php?action=query",
            // Other sites, including look-alikes.
            "https://example.com/wiki/Main_Page",
            "https://notwikipedia.org/wiki/X",
            "https://wikipedia.org.example.com/wiki/X",
            "https://en.wikipedia.org@example.com/wiki/X",
            "file:///wiki/X",
        ] {
            assert_eq!(fetch_url(url), url, "{url}");
        }
    }

    #[test]
    fn a_skin_parameter_in_the_fragment_does_not_count() {
        assert_eq!(
            fetch_url("https://en.wikipedia.org/wiki/X#useskin=a"),
            "https://en.wikipedia.org/wiki/X?useskin=vector#useskin=a"
        );
    }
}

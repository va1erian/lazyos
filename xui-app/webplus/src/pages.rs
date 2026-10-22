//! LazyWebPlus's own pages: `about:history`, `about:downloads` and `about:lazyweb`,
//! built as HTML and shown as `data:` URLs.
//!
//! Their buttons are links to `x-lazyweb:` commands. The engine cannot open that
//! scheme, so the navigation policy hands the link back to the window
//! ([`Command::parse`], `policy.rs`), which obeys it only while one of these
//! pages is on show and only for a click: a web page linking to
//! `x-lazyweb:clear-history` gets nothing.

use crate::blocker;
use crate::notify::{Decision, Site};
use crate::visits::Visit;

/// The pages' addresses as the address bar shows them.
pub const HISTORY: &str = "about:history";
pub const DOWNLOADS: &str = "about:downloads";
pub const ABOUT: &str = "about:lazyweb";
pub const BLOCKER: &str = "about:blocker";

/// The scheme of the pages' commands.
pub(crate) const SCHEME: &str = "x-lazyweb:";

/// A command one of the pages asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ClearHistory,
    /// Open download `n` (an index into the downloads list) with its app.
    OpenDownload(usize),
    /// Cancel download `n`.
    CancelDownload(usize),
    /// Forget what the person decided about a site's notifications (an origin as
    /// `notify::Origin::key` writes it).
    ForgetNotify(String),
    /// The ad blocker on or off, for every site.
    BlockerEnabled(bool),
    /// Stop blocking on `site` (`excepted`), or block there again.
    Site {
        site: String,
        excepted: bool,
    },
    /// A filter list on or off.
    List {
        id: String,
        on: bool,
    },
    /// Fetch the lists now.
    UpdateLists,
    /// Reload the page browsed last once, without blocking.
    ReloadUnblocked,
    /// Go back to the page browsed last.
    OpenSite,
}

impl Command {
    /// The command an `x-lazyweb:` URL names, if it is one.
    pub fn parse(url: &str) -> Option<Command> {
        let rest = url.strip_prefix(SCHEME)?;
        let (verb, arg) = rest.split_once('/').unwrap_or((rest, ""));
        match verb {
            "clear-history" => Some(Command::ClearHistory),
            "open" => arg.parse().ok().map(Command::OpenDownload),
            "cancel" => arg.parse().ok().map(Command::CancelDownload),
            "forget-notify" => percent_decode(arg)
                .filter(|origin| !origin.is_empty())
                .map(Command::ForgetNotify),
            "blocker-on" => Some(Command::BlockerEnabled(true)),
            "blocker-off" => Some(Command::BlockerEnabled(false)),
            "site-off" | "site-on" => blocker::normalize_site(arg).map(|site| Command::Site {
                site,
                excepted: verb == "site-off",
            }),
            "list-on" | "list-off" => blocker::lists::spec(arg).map(|spec| Command::List {
                id: spec.id.to_owned(),
                on: verb == "list-on",
            }),
            "update-lists" => Some(Command::UpdateLists),
            "reload-unblocked" => Some(Command::ReloadUnblocked),
            "open-site" => Some(Command::OpenSite),
            _ => None,
        }
    }
}

/// One download as the downloads page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRow {
    pub name: String,
    pub url: String,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    Running,
    Done,
    Failed(String),
}

/// The pages' shared look: system colours, a plain list.
const STYLE: &str = "body{font-family:sans-serif;margin:24px 32px;color:#202124}\
h1{font-size:22px;margin:0 0 4px}p.note{color:#5f6368;margin:0 0 16px}\
table{border-collapse:collapse;width:100%}td{padding:6px 8px;border-bottom:1px solid #e0e0e0;vertical-align:top}\
td.when{color:#5f6368;white-space:nowrap;width:9em}td.url{color:#5f6368;font-size:12px}\
a.button{border:1px solid #c0c0c0;padding:2px 10px;text-decoration:none;color:#202124;background:#f4f4f4}h2{font-size:16px;margin:20px 0 6px}";

pub(crate) fn page(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title>\
<style>{STYLE}</style></head><body><h1>{}</h1>{body}</body></html>",
        escape(title),
        escape(title)
    )
}

/// The history page: the visits, newest first, each a link.
pub fn history(visits: &[Visit]) -> String {
    if visits.is_empty() {
        return page("History", "<p class=\"note\">No pages visited yet.</p>");
    }
    let mut body = format!(
        "<p class=\"note\">{} pages visited. <a class=\"button\" href=\"{SCHEME}clear-history\">Clear history</a></p><table>",
        visits.len()
    );
    for visit in visits.iter().rev() {
        let title = if visit.title.trim().is_empty() {
            &visit.url
        } else {
            &visit.title
        };
        body.push_str(&format!(
            "<tr><td class=\"when\">{}</td><td><a href=\"{}\">{}</a><br><span class=\"url\">{}</span></td></tr>",
            date_time(visit.time),
            escape(&visit.url),
            escape(title),
            escape(&visit.url),
        ));
    }
    body.push_str("</table>");
    page("History", &body)
}

/// The downloads page: each download with its state and what can be done
/// with it. `folder` is where they are saved.
pub fn downloads(rows: &[DownloadRow], folder: &str) -> String {
    let mut body = format!(
        "<p class=\"note\">Downloads are saved in {}.</p>",
        escape(folder)
    );
    if rows.is_empty() {
        body.push_str("<p>Nothing downloaded yet.</p>");
        return page("Downloads", &body);
    }
    body.push_str("<table>");
    for (n, row) in rows.iter().enumerate().rev() {
        let (state, action) = match &row.state {
            DownloadState::Running => (
                format!("Downloading, {}", progress(row.received, row.total)),
                format!("<a class=\"button\" href=\"{SCHEME}cancel/{n}\">Cancel</a>"),
            ),
            DownloadState::Done => (
                size(row.received),
                format!("<a class=\"button\" href=\"{SCHEME}open/{n}\">Open</a>"),
            ),
            DownloadState::Failed(why) => (format!("Failed: {}", escape(why)), String::new()),
        };
        body.push_str(&format!(
            "<tr><td><b>{}</b><br><span class=\"url\">{}</span></td><td class=\"when\">{state}</td><td>{action}</td></tr>",
            escape(&row.name),
            escape(&row.url),
        ));
    }
    body.push_str("</table>");
    page("Downloads", &body)
}

/// The About page, with the sites whose notifications the person decided about.
pub fn about(version: &str, sites: &[Site]) -> String {
    let mut body = format!(
        "<p>Version {}. The LazyOS browser, on the \
<a href=\"https://webkit.org/\">WebKit</a> engine: JavaScript and the modern web, \
HTTPS through the system's certificate bundle.</p>\
<p class=\"note\">The process that runs a page is sandboxed: it can read the fonts and \
nothing else, has no network and no access to your files. The network process keeps \
cookies in .apps/os.lazy.lazywebplus and saves downloads to your Downloads folder.</p>\
<p class=\"note\">WebKit is free software under the LGPL 2.1 and BSD licences; this \
window is under the GPL 2.0; fonts: Liberation (SIL OFL 1.1).</p>",
        escape(version)
    );
    body.push_str(&notification_sites(sites));
    page("About LazyWeb+", &body)
}

/// The notification decisions, each with the button that forgets it (the site may ask again).
fn notification_sites(sites: &[Site]) -> String {
    let mut html = String::from("<h2>Notifications</h2>");
    if sites.is_empty() {
        html.push_str(
            "<p class=\"note\">No site has asked to show notifications. A site that does asks first, in a bar above its page.</p>",
        );
        return html;
    }
    html.push_str(
        "<p class=\"note\">Sites you allowed show notifications as toasts under this browser's name; sites you blocked cannot ask again until you forget them here.</p><table>",
    );
    for site in sites.iter().rev() {
        let word = match site.decision {
            Decision::Allow => "Allowed",
            Decision::Block => "Blocked",
        };
        html.push_str(&format!(
            "<tr><td><b>{}</b></td><td class=\"when\">{word}</td><td class=\"when\">{}</td><td><a class=\"button\" href=\"{SCHEME}forget-notify/{}\">Forget</a></td></tr>",
            escape(&site.origin),
            date_time(site.time),
            percent_encode(&site.origin),
        ));
    }
    html.push_str("</table>");
    html
}

mod text;
pub use text::{date_time, escape, percent_decode, percent_encode, progress, size};

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(time: u64, url: &str, title: &str) -> Visit {
        Visit {
            time,
            url: url.into(),
            title: title.into(),
        }
    }

    #[test]
    fn history_lists_visits_newest_first_and_escapes_them() {
        let html = history(&[
            visit(0, "http://a.test/", "First"),
            visit(1_791_257_400, "http://b.test/?q=\"x\"", "<script>"),
        ]);
        let a = html.find("http://a.test/").unwrap();
        let b = html.find("http://b.test/").unwrap();
        assert!(b < a, "newest first");
        assert!(html.contains("&lt;script&gt;") && !html.contains("<script>"));
        assert!(html.contains("q=&quot;x&quot;"));
        assert!(html.contains("1970-01-01 00:00"));
        assert!(html.contains("2026-10-06 03:30"));
        assert!(html.contains("x-lazyweb:clear-history"));
        assert!(history(&[]).contains("No pages visited"));
    }

    #[test]
    fn downloads_offer_what_each_state_allows() {
        let row = |state| DownloadRow {
            name: "a.zip".into(),
            url: "http://x/a.zip".into(),
            received: 2048,
            total: Some(4096),
            state,
        };
        let html = downloads(
            &[
                row(DownloadState::Done),
                row(DownloadState::Running),
                row(DownloadState::Failed("Cancelled".into())),
            ],
            "/home/u/Downloads",
        );
        assert!(html.contains("x-lazyweb:open/0"));
        assert!(html.contains("x-lazyweb:cancel/1"));
        assert!(!html.contains("x-lazyweb:open/2") && !html.contains("x-lazyweb:cancel/2"));
        assert!(html.contains("2.0 KB of 4.0 KB (50%)"));
        assert!(html.contains("Failed: Cancelled"));
    }

    #[test]
    fn commands_parse() {
        assert_eq!(
            Command::parse("x-lazyweb:clear-history"),
            Some(Command::ClearHistory)
        );
        assert_eq!(
            Command::parse("x-lazyweb:open/3"),
            Some(Command::OpenDownload(3))
        );
        assert_eq!(
            Command::parse("x-lazyweb:cancel/0"),
            Some(Command::CancelDownload(0))
        );
        for url in [
            "x-lazyweb:open/x",
            "x-lazyweb:rm",
            "mailto:a@b",
            "x-lazyweb:open/-1",
            "x-lazyweb:site-off/",
            "x-lazyweb:site-off/not a site",
            "x-lazyweb:list-on/nosuchlist",
        ] {
            assert_eq!(Command::parse(url), None, "{url}");
        }
    }

    #[test]
    fn the_about_page_lists_notification_decisions_each_with_a_forget_button() {
        let site = |origin: &str, decision, time| Site {
            origin: origin.into(),
            decision,
            time,
        };
        let html = about(
            "1.0",
            &[
                site("https://a.test", Decision::Allow, 0),
                site("http://b.test:8080", Decision::Block, 86_400),
            ],
        );
        assert!(html.contains("Notifications"));
        assert!(html.contains("Allowed") && html.contains("Blocked"));
        assert!(html.contains("x-lazyweb:forget-notify/https%3A%2F%2Fa.test"));
        assert!(html.contains("x-lazyweb:forget-notify/http%3A%2F%2Fb.test%3A8080"));
        assert!(html.contains("1970-01-02 00:00"));
        assert!(about("1.0", &[]).contains("No site has asked"));
    }

    #[test]
    fn forgetting_a_site_is_a_command_naming_its_origin() {
        assert_eq!(
            Command::parse("x-lazyweb:forget-notify/https%3A%2F%2Fa.test"),
            Some(Command::ForgetNotify("https://a.test".into()))
        );
        for url in [
            "x-lazyweb:forget-notify/",
            "x-lazyweb:forget-notify/%zz",
            "x-lazyweb:forget-notify/%FF",
            "x-lazyweb:forget-notify",
        ] {
            assert_eq!(Command::parse(url), None, "{url}");
        }
    }

    #[test]
    fn percent_encoding_round_trips() {
        for text in ["https://a.test:8443", "x y%z", "é☃", ""] {
            assert_eq!(percent_decode(&percent_encode(text)).as_deref(), Some(text));
        }
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    #[test]
    fn the_blockers_commands_parse() {
        assert_eq!(
            Command::parse("x-lazyweb:blocker-off"),
            Some(Command::BlockerEnabled(false))
        );
        assert_eq!(
            Command::parse("x-lazyweb:blocker-on"),
            Some(Command::BlockerEnabled(true))
        );
        assert_eq!(
            Command::parse("x-lazyweb:site-off/WWW.News.Example"),
            Some(Command::Site {
                site: "news.example".into(),
                excepted: true
            })
        );
        assert_eq!(
            Command::parse("x-lazyweb:site-on/news.example"),
            Some(Command::Site {
                site: "news.example".into(),
                excepted: false
            })
        );
        assert_eq!(
            Command::parse("x-lazyweb:list-off/easylist"),
            Some(Command::List {
                id: "easylist".into(),
                on: false
            })
        );
        assert_eq!(
            Command::parse("x-lazyweb:update-lists"),
            Some(Command::UpdateLists)
        );
        assert_eq!(
            Command::parse("x-lazyweb:reload-unblocked"),
            Some(Command::ReloadUnblocked)
        );
        assert_eq!(
            Command::parse("x-lazyweb:open-site"),
            Some(Command::OpenSite)
        );
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(progress(10, None), "10 bytes");
    }
}

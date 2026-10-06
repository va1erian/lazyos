//! LazyWeb's own pages: `about:history`, `about:downloads` and `about:lazyweb`,
//! built as HTML and shown as `data:` URLs (NetSurf reads those itself).
//!
//! Their buttons are links to `x-lazyweb:` commands. NetSurf cannot fetch that
//! scheme, so it hands the link back to the window ([`Command::parse`]),
//! which obeys it only while one of these pages is on show: a web page
//! linking to `x-lazyweb:clear-history` gets nothing.

use crate::visits::Visit;

/// The pages' addresses as the address bar shows them.
pub const HISTORY: &str = "about:history";
pub const DOWNLOADS: &str = "about:downloads";
pub const ABOUT: &str = "about:lazyweb";

/// The scheme of the pages' commands.
const SCHEME: &str = "x-lazyweb:";

/// A command one of the pages asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ClearHistory,
    /// Open download `n` (an index into the downloads list) with its app.
    OpenDownload(usize),
    /// Cancel download `n`.
    CancelDownload(usize),
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
a.button{border:1px solid #c0c0c0;padding:2px 10px;text-decoration:none;color:#202124;background:#f4f4f4}";

fn page(title: &str, body: &str) -> String {
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

/// The About page.
pub fn about(version: &str) -> String {
    let body = format!(
        "<p>Version {}. A web browser for LazyOS on the \
<a href=\"https://www.netsurf-browser.org/\">NetSurf</a> browser core, \
with HTTPS through rustls.</p><p class=\"note\">NetSurf is free software \
under the GNU General Public License, version 2; so is LazyWeb.</p>",
        escape(version)
    );
    page("About LazyWeb", &body)
}

/// `received` of `total` bytes, as people read it.
pub fn progress(received: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "{} of {} ({}%)",
            size(received),
            size(total),
            received.min(total) * 100 / total
        ),
        _ => size(received),
    }
}

/// A byte count as people read it.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `text` safe inside HTML text and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Seconds since the epoch as `YYYY-MM-DD HH:MM` (UTC: LazyOS keeps no
/// time zone).
pub fn date_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

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
        ] {
            assert_eq!(Command::parse(url), None, "{url}");
        }
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(progress(10, None), "10 bytes");
    }
}

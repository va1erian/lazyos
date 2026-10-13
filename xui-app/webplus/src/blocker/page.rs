//! `about:blocker`: the switch, this site, the lists and the sites left alone, as a page of the
//! browser's own. Its buttons are `x-lazyweb:` commands (`pages::Command`), which the browser
//! obeys only while this page is on show and only for a click.

use super::{ListInfo, ListState, Settings};
use crate::pages::{date_time, escape, page, SCHEME};

/// What the page shows.
pub struct Report {
    pub settings: Settings,
    pub lists: Vec<ListInfo>,
    /// The site of the web page browsed last: what "this site" means from here.
    pub site: Option<String>,
    /// Loads the blocker stopped on the page browsed last.
    pub blocked: u32,
}

/// The page, as HTML.
pub fn blocker(report: &Report) -> String {
    let mut body = String::new();
    let on = report.settings.enabled;
    body.push_str(&format!(
        "<p class=\"note\">Ad blocking is <b>{}</b>. {}</p>",
        if on { "on" } else { "off" },
        if report.blocked == 0 {
            String::from("Nothing was blocked on the last page.")
        } else {
            format!("{} loads were blocked on the last page.", report.blocked)
        }
    ));
    body.push_str(&format!(
        "<p><a class=\"button\" href=\"{SCHEME}blocker-{}\">Turn ad blocking {}</a></p>",
        if on { "off" } else { "on" },
        if on { "off" } else { "on" },
    ));

    if let Some(site) = &report.site {
        body.push_str("<h2>This site</h2>");
        let excepted = report
            .settings
            .exceptions
            .iter()
            .any(|e| site == e || site.ends_with(&format!(".{e}")));
        let name = escape(site);
        if excepted {
            body.push_str(&format!(
                "<p>{name}: ad blocking is <b>off</b> for this site. \
<a class=\"button\" href=\"{SCHEME}site-on/{name}\">Block ads on {name} again</a></p>"
            ));
        } else {
            body.push_str(&format!(
                "<p>{name}: ad blocking is <b>{}</b>. If the page does not work, \
<a class=\"button\" href=\"{SCHEME}site-off/{name}\">Turn off for {name}</a> \
<a class=\"button\" href=\"{SCHEME}reload-unblocked\">Reload once without blocking</a></p>",
                if on { "on" } else { "off" }
            ));
        }
        body.push_str(&format!(
            "<p><a class=\"button\" href=\"{SCHEME}open-site\">Go back to {name}</a></p>"
        ));
    }

    body.push_str("<h2>Lists</h2><table>");
    for list in &report.lists {
        body.push_str(&format!(
            "<tr><td><b>{}</b><br><span class=\"url\">{}<br>Licence: {}</span></td><td>{}</td><td>{}</td></tr>",
            escape(list.name),
            escape(list.url),
            escape(list.licence),
            escape(&state_text(list)),
            list_actions(list),
        ));
    }
    body.push_str("</table>");

    body.push_str("<h2>Sites without ad blocking</h2>");
    if report.settings.exceptions.is_empty() {
        body.push_str("<p class=\"note\">None.</p>");
    } else {
        body.push_str("<table>");
        for site in &report.settings.exceptions {
            let site = escape(site);
            body.push_str(&format!(
                "<tr><td>{site}</td><td><a class=\"button\" href=\"{SCHEME}site-on/{site}\">Block ads again</a></td></tr>"
            ));
        }
        body.push_str("</table>");
    }

    body.push_str(
        "<p class=\"note\">Blocked requests are never made, so a page loads less. Element hiding \
(the part of a list that only hides boxes) is not used. The lists are downloaded by a separate \
process that sends no cookies, and kept in this browser's folder.</p>",
    );
    page("Ad blocker", &body)
}

fn state_text(list: &ListInfo) -> String {
    if !list.enabled {
        return String::from("Off");
    }
    match &list.state {
        ListState::Waiting => String::from("Waiting for the first download"),
        ListState::Fetching => String::from("Downloading..."),
        ListState::Compiling => String::from("Preparing..."),
        ListState::Ready => {
            let mut text = format!("Blocking with {} rules", list.rules);
            if list.dropped > 0 {
                text.push_str(&format!(" ({} filters not used)", list.dropped));
            }
            if let Some(when) = list.fetched {
                text.push_str(&format!(". Updated {}", date_time(when)));
            }
            text
        }
        ListState::Failed(why) => format!("Not working: {why}"),
        ListState::Unavailable => {
            String::from("This build of the browser cannot block (no content extensions)")
        }
    }
}

fn list_actions(list: &ListInfo) -> String {
    if list.enabled {
        format!(
            "<a class=\"button\" href=\"{SCHEME}list-off/{}\">Turn off</a> \
<a class=\"button\" href=\"{SCHEME}update-lists\">Update now</a>",
            escape(list.id)
        )
    } else {
        format!(
            "<a class=\"button\" href=\"{SCHEME}list-on/{}\">Turn on</a>",
            escape(list.id)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocker::lists::EASYLIST;

    fn list(state: ListState) -> ListInfo {
        ListInfo {
            id: EASYLIST.id,
            name: EASYLIST.name,
            url: EASYLIST.url,
            licence: EASYLIST.licence,
            enabled: true,
            state,
            rules: 56_926,
            dropped: 24_851,
            fetched: Some(1_791_000_000),
        }
    }

    fn report() -> Report {
        Report {
            settings: Settings::default(),
            lists: vec![list(ListState::Ready)],
            site: Some("news.example".into()),
            blocked: 12,
        }
    }

    #[test]
    fn the_page_offers_the_switch_the_site_and_the_lists() {
        let html = blocker(&report());
        assert!(html.contains("Ad blocking is <b>on</b>"));
        assert!(html.contains("12 loads were blocked"));
        assert!(html.contains("x-lazyweb:blocker-off"));
        assert!(html.contains("x-lazyweb:site-off/news.example"));
        assert!(html.contains("x-lazyweb:reload-unblocked"));
        assert!(html.contains("x-lazyweb:open-site"));
        assert!(html.contains("Blocking with 56926 rules (24851 filters not used)"));
        assert!(html.contains("Updated 2026-10-0"));
        assert!(html.contains("x-lazyweb:list-off/easylist") && html.contains("x-lazyweb:update-lists"));
        assert!(html.contains("GPL-3.0-or-later or CC BY-SA 3.0"));
    }

    #[test]
    fn an_excepted_site_offers_to_block_again_and_is_listed() {
        let mut r = report();
        r.settings.exceptions.insert("example".into());
        r.site = Some("news.example".into());
        let html = blocker(&r);
        assert!(html.contains("ad blocking is <b>off</b> for this site"));
        assert!(html.contains("x-lazyweb:site-on/news.example"));
        assert!(html.contains("x-lazyweb:site-on/example"), "the exception itself is listed");
        assert!(!html.contains("x-lazyweb:site-off/"));
    }

    #[test]
    fn a_switched_off_blocker_and_list_offer_to_turn_on() {
        let mut r = report();
        r.settings.enabled = false;
        r.lists[0].enabled = false;
        r.site = None;
        r.blocked = 0;
        let html = blocker(&r);
        assert!(html.contains("x-lazyweb:blocker-on"));
        assert!(html.contains("x-lazyweb:list-on/easylist"));
        assert!(html.contains("Nothing was blocked"));
        assert!(!html.contains("This site"));
    }

    #[test]
    fn what_goes_wrong_is_said_and_text_is_escaped() {
        let mut r = report();
        r.lists[0].state = ListState::Failed("<b>HTTP 503</b>".into());
        r.site = Some("a.example".into());
        let html = blocker(&r);
        assert!(html.contains("Not working: &lt;b&gt;HTTP 503&lt;/b&gt;"));
        assert!(!html.contains("<b>HTTP 503"));
        r.lists[0].state = ListState::Unavailable;
        assert!(blocker(&r).contains("cannot block"));
        r.lists[0].state = ListState::Fetching;
        assert!(blocker(&r).contains("Downloading"));
    }
}

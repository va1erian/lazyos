//! The toolbar's ad-blocker button: its icon and tooltip say what blocking does for the page on show.

use xui_core::icon::Lucide;

use super::Browser;
use crate::blocker::{self, ListState};

impl Browser {
    /// Shows what the ad blocker does for the page on show: the icon is the eye that sees nothing
    /// while it blocks, and the tooltip says how much it stopped.
    pub(super) fn update_shield(&self) {
        let site = self.site_url.as_deref().and_then(blocker::site_of);
        let enabled = self.blocker.settings().enabled;
        let excepted = site
            .as_deref()
            .is_some_and(|site| self.blocker.site_excepted(site));
        let applies = enabled && !excepted;
        let ready = self.blocker.any_ready();
        let blocked = self.blocker.blocked();
        let key = (blocked, applies, ready);
        if self.shield_shown.replace(Some(key)) == Some(key) {
            return;
        }
        let (icon, tip) = if !enabled {
            (Lucide::Eye, String::from("Ad blocker: off"))
        } else if excepted {
            let site = site.unwrap_or_default();
            (Lucide::Eye, format!("Ad blocker: off for {site}"))
        } else if !ready {
            let lists = self.blocker.lists();
            let why = match lists.iter().find(|l| l.enabled).map(|l| &l.state) {
                None => "no list is on",
                Some(ListState::Failed(_)) => "a list does not work",
                Some(ListState::Unavailable) => "not available in this build",
                Some(_) => "preparing its lists",
            };
            (Lucide::EyeOff, format!("Ad blocker: {why}"))
        } else if blocked == 0 {
            (
                Lucide::EyeOff,
                String::from("Ad blocker: on, nothing blocked here"),
            )
        } else {
            (
                Lucide::EyeOff,
                format!("Ad blocker: {blocked} blocked on this page"),
            )
        };
        self.shield.set_icon(Some(icon));
        let _ = self.shield.set_tooltip(&tip);
    }
}

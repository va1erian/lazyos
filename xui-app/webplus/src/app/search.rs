//! What typed text does: a search, an address over HTTPS first, and the question that follows when a
//! host does not answer over HTTPS (`crate::downgrade`).
//!
//! Serial evidence: `WEBPLUS:HTTPS:ASK` when the question is shown, `WEBPLUS:HTTPS:ALLOW` when the
//! person loads the host without encryption (the host itself is named only in a UI-probe image:
//! `WEBPLUS:HTTPS:HOST:<host>`).

use xui_core::widget::HasText;

use super::Browser;
use crate::address::Typed;
use crate::downgrade::{self, Pending};

impl Browser {
    /// Opens what the address field holds, once it has been classified.
    pub(super) fn open_typed(&mut self, typed: Typed) {
        let (url, pending) = match typed {
            Typed::Host { host, http, .. } if self.allowed_http.contains(&host) => (http, None),
            Typed::Host { host, https, http } => (https, Some(Pending { host, http })),
            Typed::Url(url) | Typed::Search(url) => (url, None),
        };
        self.address.set_text(&url);
        self.open(&url);
        self.page.focus();
        self.pending_https = pending;
    }

    /// A load failed: when it was the typed host over HTTPS and nothing answered, ask whether to try
    /// plain HTTP.
    pub(super) fn offer_http(&mut self, error: &str) {
        let Some(pending) = self.pending_https.take() else {
            return;
        };
        if !downgrade::connection_failed(error, &pending.host) {
            // A certificate error or anything else: never an offer to drop the encryption.
            return;
        }
        crate::marker("HTTPS:ASK");
        self.trace(&format!("HTTPS:HOST:{}", crate::marker_text(&pending.host)));
        self.offered = Some(pending);
        self.open(downgrade::NAME);
    }

    /// The page of the question builds from the host it asks about.
    pub(super) fn downgrade_page(&self) -> String {
        match &self.offered {
            Some(offered) => downgrade::question(&offered.host),
            None => self.internal.start(),
        }
    }

    /// The person chose to load `host` without encryption: only the host just asked about counts.
    pub(super) fn allow_http(&mut self, host: &str) {
        let Some(offered) = self.offered.take_if(|offered| offered.host == host) else {
            return;
        };
        crate::marker("HTTPS:ALLOW");
        self.allowed_http.insert(&offered.host);
        self.open(&offered.http);
    }
}

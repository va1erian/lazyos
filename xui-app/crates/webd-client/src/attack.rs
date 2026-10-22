//! A malicious client's probes against the real webd (`tools/webd/attack_run.py`, the "malicious
//! client" half of webd's attack harness; docs/webd-plan.md P5). The client `os.lazy.webdattack`
//! lists **only** `os.lazy.webd.view.v1` in its manifest (not the `net` or `browser` scope), so a
//! `CreateView` it sends through a scope it was not granted is refused by the *kernel* before webd
//! sees it: that gate cannot be reproduced by the host fabric (which delivers any interface id), so
//! it is proven only in QEMU.
//!
//! Each probe drives a [`FullWebView`] over a fresh [`ScopedTransport`] and records a one-word
//! outcome; the judge (`tools/webd/attackjudge.py`) holds the expectation table and decides
//! pass/fail, as `tools/accounts` and `tools/webkit/attackjudge.py` do. The resource and flood
//! probes are also host-tested (`tests/attack.rs`), which the fabric reproduces; the scope-gate
//! probes are judged only from a guest run.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::event::Error;
use crate::full::{FullWebView, Scope};
use crate::transport::ScopedTransport;

/// One probe's name and the outcome word the judge classifies (`OK`, `REFUSED:<errno>`,
/// `NOTRUNNING`, `LOST`, and for the cap `REFUSED:<errno>:<count live when refused>`).
pub struct Probe {
    pub name: &'static str,
    pub outcome: String,
}

/// Views one client may hold at once (the service's `VIEWS_PER_CLIENT`); the cap probe makes one past
/// it. Kept here so the judge and the probe agree without a cross-crate dependency.
pub const VIEWS_PER_CLIENT: usize = 8;

fn outcome<T>(result: Result<T, Error>) -> String {
    match result {
        Ok(_) => "OK".to_owned(),
        Err(Error::Refused(code)) => format!("REFUSED:{code}"),
        Err(Error::NotRunning) => "NOTRUNNING".to_owned(),
        Err(Error::Lost(_)) => "LOST".to_owned(),
        Err(Error::NoView) => "NOVIEW".to_owned(),
    }
}

fn web() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_WEB,
        ..wire::ViewConfig::default()
    }
}

fn browser() -> wire::ViewConfig {
    wire::ViewConfig {
        profile: "browser".into(),
        ..web()
    }
}

fn data_only() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_DATA_ONLY,
        ..wire::ViewConfig::default()
    }
}

/// Run the malicious-client probes; `make` builds a fresh transport bound to the full service for
/// each view. Returns the outcomes in a fixed order.
pub fn run<T: ScopedTransport>(mut make: impl FnMut() -> T) -> Vec<Probe> {
    const W: u32 = 320;
    const H: u32 = 240;
    let mut probes = Vec::new();
    let mut say = |name, outcome| probes.push(Probe { name, outcome });

    // 1-2. A `CreateView` through a scope the manifest does not grant: the kernel refuses the call
    //      (EACCES/EPERM) and prints a LABEL:DENY for this client's label and the scope's interface.
    let mut net = FullWebView::new(make(), Scope::Net, web());
    say("scope_net", outcome(net.create(W, H)));
    net.destroy();
    let mut browser = FullWebView::new(make(), Scope::Browser, browser());
    say("scope_browser", outcome(browser.create(W, H)));
    browser.destroy();

    // 3. The granted scope (`view`, DataOnly): a view is made. It is kept for the flood probe.
    let mut view = FullWebView::new(make(), Scope::View, data_only());
    let created = view.create(W, H);
    say("view_ok", outcome(created));

    // 4. Flood: a hostile client can send input with no bound (oneway calls). webd must stay up and
    //    keep answering. 1000 mouse moves, then a call that needs a reply: it must still come back.
    for i in 0..1000i32 {
        view.mouse(crate::MouseKind::Move, -1, i % W as i32, i % H as i32, 0);
    }
    say(
        "flood_alive",
        outcome(view.load_html("<title>a</title>", "about:blank")),
    );

    // 5. The view-count cap: a client may hold VIEWS_PER_CLIENT views; one more is refused. The views
    //    are kept alive so the cap is really reached (the one from probe 3 counts).
    let mut held = vec![view];
    let mut cap_outcome = String::from("NOTREACHED");
    for _ in 0..(VIEWS_PER_CLIENT + 2) {
        let mut extra = FullWebView::new(make(), Scope::View, data_only());
        match extra.create(W, H) {
            Ok(()) => held.push(extra),
            Err(Error::Refused(code)) => {
                cap_outcome = format!("REFUSED:{code}:{}", held.len());
                break;
            }
            Err(other) => {
                cap_outcome = outcome::<()>(Err(other));
                break;
            }
        }
    }
    say("view_cap", cap_outcome);
    for mut v in held {
        v.destroy();
    }

    // 6. After the flood and the cap, webd still serves: a fresh view is made.
    let mut again = FullWebView::new(make(), Scope::View, data_only());
    say("alive_after", outcome(again.create(W, H)));
    again.destroy();

    probes
}

/// The outcome of the probe named `name`, or `None`.
pub fn outcome_of<'a>(probes: &'a [Probe], name: &str) -> Option<&'a str> {
    probes
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.outcome.as_str())
}

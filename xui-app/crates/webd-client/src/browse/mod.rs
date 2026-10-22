//! The scenario of the test client `webdbrowse` (`os.lazy.webdbrowse`): it drives full views of
//! webd through each scope against the harness server and says what happened, one [`Marker`] per
//! step, so the host tests (against the service half and a fake web) and the QEMU harness judge
//! the same story. The app's window only paints view A's picture and forwards input to it.
//!
//! The steps, in order (`docs/webd-plan.md` P2 slice 3): `VIEW` (view A through
//! `os.lazy.webd.browser.v1`), `BLOCKLIST`, `NAV` (with `BLOCKED` and `UNBLOCKED` on the index page),
//! `BACK`, `FORWARD`, `COOKIE_A`, `COOKIE_B` (view B through `os.lazy.webd.net.v1`, its own profile),
//! `COOKIE_A2` (view C, the browser profile again), `DOWNLOAD`, `DIALOG`, `POPUP`, `HELD`, `SCOPE`
//! (`os.lazy.webd.view.v1`: no network), `DELTA`, `LATENCY`, then `DONE`. A step that fails says
//! why and the next one runs; only a view A that cannot be made ends the run at once.

mod more;
mod order;
mod steps;
mod timing;

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::event::{Error, Lost};
use crate::full::{FullEvent, FullWebView, Scope, ViewConfig};
use crate::picture::Picture;
use crate::transport::ScopedTransport;
use order::{next_stage, stage_uses_a, step_name, Stage};

/// The harness server, as QEMU's user network shows the host.
pub const DEFAULT_BASE: &str = "http://10.0.2.2:18081";
/// The rule list the scenario compiles (`BLOCKLIST`): the index page's `/blocked.png`.
pub const RULES: &str = r#"[{"trigger":{"url-filter":"blocked\\.png"},"action":{"type":"block"}}]"#;
pub const LIST: &str = "test";
/// Milliseconds between tries to make view A while webd is not there.
pub const RETRY_MS: u64 = 250;

/// The server's address: the first argument that is an `http://` URL, else [`DEFAULT_BASE`];
/// without a trailing `/`.
pub fn base_url(args: impl IntoIterator<Item = String>) -> String {
    let base = args
        .into_iter()
        .find(|arg| arg.starts_with("http://"))
        .unwrap_or_else(|| DEFAULT_BASE.to_owned());
    base.trim_end_matches('/').to_owned()
}

/// View A: the browser profile, the network, every question and deltas.
pub fn browser_config() -> ViewConfig {
    ViewConfig {
        profile: "browser".into(),
        scheme_policy: wire::SCHEME_POLICY_WEB,
        scripts: true,
        handoff: wire::HANDOFF_ASK,
        downloads: true,
        popups: true,
        blocking: true,
        deltas: true,
        dialogs: true,
        ..ViewConfig::default()
    }
}

/// View B: the network with the app's own profile, whole frames only.
pub fn own_config() -> ViewConfig {
    ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_WEB,
        scripts: true,
        handoff: wire::HANDOFF_DENY,
        ..ViewConfig::default()
    }
}

/// View C: the browser profile again, nothing else.
pub fn browser_plain_config() -> ViewConfig {
    ViewConfig {
        profile: "browser".into(),
        ..own_config()
    }
}

/// View D: no network.
pub fn data_config() -> ViewConfig {
    ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_DATA_ONLY,
        handoff: wire::HANDOFF_DENY,
        ..ViewConfig::default()
    }
}

/// One line of evidence: `WEBDBROWSE:<step>:PASS[:<detail>]` or `WEBDBROWSE:<step>:FAIL:<why>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub step: &'static str,
    pub pass: bool,
    pub detail: String,
}

impl Marker {
    /// The line after the `WEBDBROWSE:` prefix.
    pub fn text(&self) -> String {
        let verdict = if self.pass { "PASS" } else { "FAIL" };
        match self.detail.is_empty() {
            true => format!("{}:{verdict}", self.step),
            false => format!("{}:{verdict}:{}", self.step, self.detail),
        }
    }
}

/// Which view an event or a load is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    A,
    Other,
}

/// An applied frame of view A, when it arrived.
#[derive(Clone, Copy, Debug)]
struct FrameSeen {
    at: u64,
    whole: bool,
    dy: i32,
    rects: usize,
}

/// What the views said since a step began.
#[derive(Debug, Default)]
struct Seen {
    blocked: u32,
    lists: Vec<(String, u32)>,
    dialogs: Vec<u32>,
    new_windows: Vec<(String, bool)>,
    /// `(url, kind, gesture)` of held navigations.
    held: Vec<(String, u32, bool)>,
    handed_off: Vec<String>,
    download_name: Option<String>,
    download_received: i64,
    download_done: Option<String>,
    download_failed: Option<String>,
    frames: Vec<FrameSeen>,
    lost_a: Option<Lost>,
}

/// The scenario, driven by [`Browse::tick`] from the app's timer.
pub struct Browse<T: ScopedTransport> {
    make: Box<dyn FnMut() -> T>,
    base: String,
    a: Option<FullWebView<T>>,
    other: Option<FullWebView<T>>,
    size: (u32, u32),
    stage: Stage,
    phase: u32,
    stage_at: u64,
    /// A time a phase noted (a send, a settle start).
    mark: u64,
    retry_at: u64,
    /// `page.finished` and the committed address when the last navigation was asked for.
    finished_at: u32,
    url_at: String,
    seen: Seen,
    results: Vec<(&'static str, bool)>,
    /// What a step keeps for its detail.
    index_url: String,
    nav_failed: bool,
    first_title: String,
    timing: timing::Timing,
}

impl<T: ScopedTransport> Browse<T> {
    /// A scenario against the server at `base`; `make` gives a fresh transport for each view.
    pub fn new(make: impl FnMut() -> T + 'static, base: &str) -> Self {
        Browse {
            make: Box::new(make),
            base: base.trim_end_matches('/').to_owned(),
            a: None,
            other: None,
            size: (0, 0),
            stage: Stage::View,
            phase: 0,
            stage_at: 0,
            mark: 0,
            retry_at: 0,
            finished_at: 0,
            url_at: String::new(),
            seen: Seen::default(),
            results: Vec::new(),
            index_url: String::new(),
            nav_failed: false,
            first_title: String::new(),
            timing: timing::Timing::default(),
        }
    }

    /// View A, for input; `None` before it is made.
    pub fn view(&mut self) -> Option<&mut FullWebView<T>> {
        self.a.as_mut()
    }

    /// View A's picture; empty before its first frame.
    pub fn picture(&self) -> Option<&Picture> {
        self.a.as_ref().map(FullWebView::picture)
    }

    /// The scenario printed `DONE`.
    pub fn done(&self) -> bool {
        matches!(self.stage, Stage::Idle)
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// One turn: `size` is the area view A's picture fills, `now_ms` any monotonic clock.
    pub fn tick(&mut self, size: (u32, u32), now_ms: u64) -> Vec<Marker> {
        let mut out = Vec::new();
        if size.0 == 0 || size.1 == 0 {
            return out;
        }
        if self.stage == Stage::View {
            self.size = size;
        }
        self.observe(Which::A, now_ms);
        self.observe(Which::Other, now_ms);
        if self.stage != Stage::View && self.stage < Stage::Done && !self.a_live() {
            self.a_gone(now_ms, &mut out);
        }
        self.step(size, now_ms, &mut out);
        out
    }

    fn step(&mut self, size: (u32, u32), now: u64, out: &mut Vec<Marker>) {
        match self.stage {
            Stage::View => self.stage_view(now, out),
            Stage::Blocklist => self.stage_blocklist(now, out),
            Stage::Index => self.stage_index(now, out),
            Stage::Blocked => self.stage_blocked(now, out),
            Stage::Unblocked => self.stage_unblocked(now, out),
            Stage::Page2 => self.stage_page2(now, out),
            Stage::Back => self.stage_history(now, out, true),
            Stage::Forward => self.stage_history(now, out, false),
            Stage::CookieA => self.stage_cookie_a(now, out),
            Stage::CookieB => self.stage_cookie_b(now, out),
            Stage::CookieA2 => self.stage_cookie_a2(now, out),
            Stage::Download => self.stage_download(now, out),
            Stage::Dialog => self.stage_dialog(now, out),
            Stage::Popup => self.stage_popup(now, out),
            Stage::Held => self.stage_held(now, out),
            Stage::Scope => self.stage_scope(now, out),
            Stage::Delta => self.stage_delta(now, out),
            Stage::Latency => self.stage_latency(now, out),
            Stage::Done => self.stage_done(now, out),
            Stage::Idle => {
                // Follow the window once the run is over.
                if let Some(a) = self.a.as_mut().filter(|a| a.is_live() && a.size() != size) {
                    let _ = a.resize(size.0, size.1);
                }
            }
        }
    }

    /// Read what each view said, answer its questions, and note what the steps look for.
    fn observe(&mut self, which: Which, now: u64) {
        let stage = self.stage;
        let view = match which {
            Which::A => self.a.as_mut(),
            Which::Other => self.other.as_mut(),
        };
        let Some(view) = view else {
            return;
        };
        let seen = &mut self.seen;
        // Only the timed steps look at frames: none is kept while the window just shows view A.
        let timed = matches!(stage, Stage::Delta | Stage::Latency);
        for event in view.poll() {
            match event {
                FullEvent::Frame(info) if info.applied && which == Which::A && timed => {
                    seen.frames.push(FrameSeen {
                        at: now,
                        whole: info.whole,
                        dy: info.shift.map_or(0, |s| s.dy),
                        rects: info.rects.len(),
                    });
                }
                FullEvent::Blocked { count } if which == Which::A => {
                    seen.blocked = seen.blocked.max(count);
                }
                FullEvent::BlockingList { list, status } => seen.lists.push((list, status)),
                FullEvent::Dialog {
                    dialog,
                    kind,
                    default_value,
                    ..
                } => {
                    let _ = view.dialog_reply(dialog, true, &default_value);
                    seen.dialogs.push(kind);
                }
                FullEvent::NavigationRequested {
                    request,
                    url,
                    kind,
                    gesture,
                } => {
                    // Only the HELD step says yes (and a `mailto:` must still not load).
                    let allow = stage == Stage::Held && which == Which::A;
                    let _ = view.navigation_answer(request, allow);
                    seen.held.push((url, kind, gesture));
                }
                FullEvent::NewWindowRequested { url, gesture } => {
                    seen.new_windows.push((url, gesture));
                }
                FullEvent::DownloadStarted { name, .. } => seen.download_name = Some(name),
                FullEvent::DownloadProgress { received, .. } => seen.download_received = received,
                FullEvent::DownloadDone { name, .. } => seen.download_done = Some(name),
                FullEvent::DownloadFailed { reason, .. } => seen.download_failed = Some(reason),
                FullEvent::NotifyPermissionRequest { origin } => {
                    let _ = view.notify_permission(&origin, wire::PERMISSION_ANSWER_DISMISS);
                }
                FullEvent::HandedOff(url) => seen.handed_off.push(url),
                FullEvent::Lost(why) if which == Which::A => seen.lost_a = Some(why),
                _ => {}
            }
        }
    }

    fn a_live(&self) -> bool {
        self.a.as_ref().is_some_and(FullWebView::is_live)
    }

    /// View A was lost mid-run: the step fails and the next one runs (and fails at once).
    fn a_gone(&mut self, now: u64, out: &mut Vec<Marker>) {
        let why = self.seen.lost_a.or_else(|| self.a.as_ref()?.lost());
        let reason = why.map_or("gone", Lost::name);
        let stage = self.stage;
        // A NAV that already failed on the index page is not reported twice.
        let said = stage == Stage::Page2 && self.nav_failed;
        if stage_uses_a(stage) && !said {
            self.fail(step_name(stage), format!("view A is lost ({reason})"), out);
            self.nav_failed |= stage == Stage::Index;
        }
        if stage_uses_a(stage) {
            self.next(next_stage(stage), now);
        }
    }

    fn pass(&mut self, step: &'static str, detail: String, out: &mut Vec<Marker>) {
        self.results.push((step, true));
        out.push(Marker {
            step,
            pass: true,
            detail,
        });
    }

    fn fail(&mut self, step: &'static str, why: String, out: &mut Vec<Marker>) {
        self.results.push((step, false));
        out.push(Marker {
            step,
            pass: false,
            detail: why,
        });
    }

    fn next(&mut self, stage: Stage, now: u64) {
        self.stage = stage;
        self.phase = 0;
        self.stage_at = now;
        self.mark = now;
    }

    fn elapsed(&self, now: u64) -> u64 {
        now.saturating_sub(self.stage_at)
    }

    fn view_ref(&self, which: Which) -> Option<&FullWebView<T>> {
        match which {
            Which::A => self.a.as_ref(),
            Which::Other => self.other.as_ref(),
        }
    }

    fn view_of(&mut self, which: Which) -> Option<&mut FullWebView<T>> {
        match which {
            Which::A => self.a.as_mut(),
            Which::Other => self.other.as_mut(),
        }
    }

    /// Ask `which` to load `url`, noting where it was.
    fn load(&mut self, which: Which, url: &str) -> Result<(), Error> {
        self.note_page(which);
        self.view_of(which).ok_or(Error::NoView)?.load(url)
    }

    fn note_page(&mut self, which: Which) {
        if let Some(page) = self.view_of(which).map(|view| view.page().clone()) {
            self.finished_at = page.finished;
            self.url_at = page.committed_url;
        }
    }

    /// The navigation asked for last has finished: `finished` grew with nothing loading, or (a
    /// page restored from history) the committed address became `expected`.
    fn loaded(&self, which: Which, expected: Option<&str>) -> bool {
        let Some(page) = self.view_ref(which).map(|view| view.page()) else {
            return false;
        };
        let restored = expected.is_some_and(|url| page.committed_url == url && url != self.url_at);
        !page.loading && (page.finished > self.finished_at || restored)
    }

    /// The title of `which`'s page, cut for a trace.
    fn title(&self, which: Which) -> String {
        let title = self
            .view_ref(which)
            .map(|view| view.page().title.clone())
            .unwrap_or_default();
        clip(&title)
    }

    fn committed(&self, which: Which) -> String {
        self.view_ref(which)
            .map(|view| view.page().committed_url.clone())
            .unwrap_or_default()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The size of a view that is not shown (B, C, D).
    fn small_size(&self) -> (u32, u32) {
        (self.size.0.min(400), self.size.1.min(300))
    }

    fn make_view(&mut self, scope: Scope, config: ViewConfig) -> FullWebView<T> {
        FullWebView::new((self.make)(), scope, config)
    }
}

/// A page's text in a trace: one line, at most 120 characters.
fn clip(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(120).collect()
}

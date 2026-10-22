//! A fake web for the full views' tests: an [`Engine`] that answers loads the way WebKit and the
//! harness server (`tools/webd`, the pages `webdbrowse` visits) would, by posting to the
//! [`EventQueue`] the service drains, synchronously, from inside the call. It keeps a history per
//! view, a cookie jar per profile, the rule lists a view compiled, and a log of what the "server"
//! was asked for, so a test can check what loaded and what did not.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::engine::{Engine, EngineConfig, EngineEvents};
use xui_webd::events::{EventQueue, PageNow};
use xui_webd::policy::Verdict;

/// The fake server's address.
pub const BASE: &str = "http://web.test:18081";
/// How tall the `/long` page is, in rows.
pub const LONG_ROWS: i32 = 3000;
/// Rows one wheel notch scrolls.
pub const NOTCH_ROWS: i32 = 40;
/// Bytes of `/file.bin`.
pub const FILE_BYTES: usize = 4096;

/// The pixel of row `row` of the `/long` page: every row is different, so a misplaced row shows.
pub fn long_row(row: i32) -> [u8; 4] {
    [(row & 0xff) as u8, ((row >> 8) & 0xff) as u8, 0x40, 0xff]
}

/// The `/long` page at scroll offset `y`, `w` x `h`.
pub fn long_frame(w: i32, h: i32, y: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let pixel = long_row(y + row);
        for _ in 0..w {
            out.extend_from_slice(&pixel);
        }
    }
    out
}

/// What the fake knows of one view.
#[derive(Default)]
struct Page {
    config: EngineConfig,
    history: Vec<String>,
    at: usize,
    finished: u32,
    title: String,
    /// The `/long` page's scroll offset.
    y: i32,
    lists: Vec<String>,
    /// The page the next dialog answer belongs to (`/alert`).
    dialog_step: u64,
    next_request: u64,
}

#[derive(Default)]
struct State {
    views: BTreeMap<u64, Page>,
    jars: BTreeMap<String, Vec<String>>,
    /// Every address the "server" was asked for, in order.
    requests: Vec<String>,
    /// `(view, request, allow)` as the service answered held navigations.
    answers: Vec<(u64, u64, bool)>,
    /// `(view, dialog, accepted)`.
    replies: Vec<(u64, u64, bool)>,
    destroyed: Vec<u64>,
    /// `(view, reload)` of `recover`, `(view, origin)` of `notify_forget`, `(view, origin, answer)`
    /// of `notify_permission`.
    recovers: Vec<(u64, bool)>,
    forgets: Vec<(u64, String)>,
    permissions: Vec<(u64, String, i32)>,
}

/// The fake. Clones share one state.
#[derive(Clone)]
pub struct FakeWeb {
    queue: Arc<EventQueue>,
    state: Arc<Mutex<State>>,
}

impl FakeWeb {
    pub fn new(queue: Arc<EventQueue>) -> FakeWeb {
        FakeWeb {
            queue,
            state: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn requests(&self) -> Vec<String> {
        self.lock().requests.clone()
    }

    pub fn answers(&self) -> Vec<(u64, u64, bool)> {
        self.lock().answers.clone()
    }

    pub fn replies(&self) -> Vec<(u64, u64, bool)> {
        self.lock().replies.clone()
    }

    pub fn config(&self, view: u64) -> Option<EngineConfig> {
        self.lock().views.get(&view).map(|p| p.config.clone())
    }

    pub fn destroyed(&self) -> Vec<u64> {
        self.lock().destroyed.clone()
    }

    pub fn recovers(&self) -> Vec<(u64, bool)> {
        self.lock().recovers.clone()
    }

    pub fn forgets(&self) -> Vec<(u64, String)> {
        self.lock().forgets.clone()
    }

    pub fn permissions(&self) -> Vec<(u64, String, i32)> {
        self.lock().permissions.clone()
    }

    /// The `/long` page's scroll offset in `view`.
    pub fn scrolled(&self, view: u64) -> i32 {
        self.lock().views.get(&view).map_or(0, |p| p.y)
    }

    /// The jar of a view's profile: the browser profile is one, an app's own is per web process
    /// group.
    fn jar(config: &EngineConfig) -> String {
        match &config.full {
            Some(full) if !full.profile.is_empty() => "browser".into(),
            _ => format!("group-{}", config.group),
        }
    }

    pub(crate) fn size(&self, view: u64) -> (i32, i32) {
        self.lock()
            .views
            .get(&view)
            .map_or((0, 0), |p| (p.config.width, p.config.height))
    }

    /// The picture of what `view` shows now.
    fn render(&self, view: u64) -> Vec<u8> {
        let (w, h) = self.size(view);
        let state = self.lock();
        let Some(page) = state.views.get(&view) else {
            return Vec::new();
        };
        if page.title == "Long" {
            return long_frame(w, h, page.y);
        }
        let shade = page
            .title
            .bytes()
            .fold(7u8, |a, b| a.wrapping_mul(31).wrapping_add(b));
        [shade, 0x80, 0xff - shade, 0xff]
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect()
    }

    /// Say where `view` is and paint it.
    pub(crate) fn show(&self, view: u64, loading: bool) {
        let page = {
            let mut state = self.lock();
            let Some(page) = state.views.get_mut(&view) else {
                return;
            };
            if !loading {
                page.finished += 1;
            }
            let url = page.history.get(page.at).cloned().unwrap_or_default();
            PageNow {
                url: url.clone(),
                title: page.title.clone(),
                loading,
                can_back: page.at > 0,
                can_forward: page.at + 1 < page.history.len(),
                progress: if loading { 100 } else { 1000 },
                error: String::new(),
                committed_url: url,
                secure: false,
                finished: page.finished,
            }
        };
        self.queue.page_state(view, page);
        if !loading {
            let (w, h) = self.size(view);
            self.queue.frame(view, &self.render(view), w, h);
        }
    }

    /// Navigate `view` to `url` as a new history entry (`push`) or in place.
    fn navigate(&self, view: u64, url: &str, push: bool) {
        let path = url.strip_prefix(BASE).unwrap_or(url).to_owned();
        if path == "/file.bin" {
            self.fetch(url);
            return self.download(view, url);
        }
        {
            let mut state = self.lock();
            let Some(page) = state.views.get_mut(&view) else {
                return;
            };
            if push {
                page.history.truncate(page.at + 1);
                page.history.push(url.to_owned());
                page.at = page.history.len() - 1;
            }
            page.y = 0;
        }
        self.fetch(url);
        super::web_pages::open(self, view, &path);
    }

    pub(crate) fn fetch(&self, url: &str) {
        self.lock().requests.push(url.to_owned());
    }

    pub(crate) fn set_title(&self, view: u64, title: &str) {
        if let Some(page) = self.lock().views.get_mut(&view) {
            page.title = title.to_owned();
        }
    }

    pub(crate) fn queue(&self) -> &EventQueue {
        &self.queue
    }

    /// The cookies `view`'s profile holds, and a new one set.
    pub(crate) fn cookies(&self, view: u64, set: Option<&str>) -> String {
        let mut state = self.lock();
        let Some(config) = state.views.get(&view).map(|p| p.config.clone()) else {
            return String::new();
        };
        let jar = state.jars.entry(Self::jar(&config)).or_default();
        if let Some(cookie) = set {
            jar.retain(|c| c.split('=').next() != cookie.split('=').next());
            jar.push(cookie.to_owned());
        }
        jar.join("; ")
    }

    pub(crate) fn has_list(&self, view: u64, list: &str) -> bool {
        self.lock()
            .views
            .get(&view)
            .is_some_and(|p| p.lists.iter().any(|l| l == list))
    }

    pub(crate) fn no_network(&self, view: u64) -> bool {
        self.lock()
            .views
            .get(&view)
            .and_then(|p| p.config.full.as_ref().map(|f| f.no_network))
            .unwrap_or(true)
    }

    pub(crate) fn set_dialog_step(&self, view: u64, step: u64) {
        if let Some(page) = self.lock().views.get_mut(&view) {
            page.dialog_step = step;
        }
    }

    /// The page asks to go to `url` by script: the view's policy decides.
    pub(crate) fn page_navigates(&self, view: u64, url: &str) -> Verdict {
        let request = {
            let mut state = self.lock();
            let Some(page) = state.views.get_mut(&view) else {
                return Verdict::Deny;
            };
            page.next_request += 1;
            page.next_request
        };
        let kind = wire::NAVIGATION_KIND_OTHER as i32;
        self.queue.navigate(view, request, url, kind, false, true)
    }

    fn download(&self, view: u64, url: &str) {
        let part = self
            .queue
            .download_start(view, 1, url, "file.bin", FILE_BYTES as i64);
        let Some(part) = part else {
            return;
        };
        if std::fs::write(&part, vec![0x5a; FILE_BYTES]).is_err() {
            return self.queue.download_fail(view, 1, "cannot write");
        }
        let bytes = FILE_BYTES as i64;
        self.queue.download_progress(view, 1, bytes, bytes);
        self.queue.download_done(view, 1);
    }

    /// The page at `/long` scrolls by `ticks_y` notches (negative: down), as a shift and the rows
    /// it uncovers.
    fn scroll_long(&self, view: u64, ticks_y: i32) {
        let (w, h) = self.size(view);
        let (old, new) = {
            let mut state = self.lock();
            let Some(page) = state.views.get_mut(&view) else {
                return;
            };
            if page.title != "Long" {
                return;
            }
            let old = page.y;
            page.y = (old - ticks_y * NOTCH_ROWS).clamp(0, LONG_ROWS - h);
            (old, page.y)
        };
        let dy = old - new;
        if dy == 0 {
            return;
        }
        let at_bottom = new == LONG_ROWS - h;
        self.queue.scroll(view, 0, new, at_bottom);
        let rect = if dy < 0 {
            [0, h + dy, w, -dy]
        } else {
            [0, 0, w, dy]
        };
        let frame = long_frame(w, h, new);
        let shift = Some([0, 0, w, h, 0, dy]);
        if dy.abs() >= h || self.queue.frame_delta(view, &frame, w, h, shift, &[rect]) {
            self.queue.frame(view, &frame, w, h);
        }
    }
}

impl Engine for FakeWeb {
    fn view_create(&self, view: u64, config: &EngineConfig) {
        let page = Page {
            config: config.clone(),
            ..Page::default()
        };
        self.lock().views.insert(view, page);
    }

    fn view_destroy(&self, view: u64) {
        let mut state = self.lock();
        state.views.remove(&view);
        state.destroyed.push(view);
    }

    fn load_html(&self, view: u64, html: &[u8], base_url: &str) {
        let html = String::from_utf8_lossy(html).into_owned();
        // A picture of the page reaches the server only when the view has a network.
        if !self.no_network(view) {
            for part in html.split("src=\"").skip(1) {
                if let Some(end) = part.find('"') {
                    self.fetch(&part[..end]);
                }
            }
        }
        let title = html
            .split("<title>")
            .nth(1)
            .and_then(|t| t.split("</title>").next())
            .unwrap_or("")
            .to_owned();
        {
            let mut state = self.lock();
            if let Some(page) = state.views.get_mut(&view) {
                page.history = vec![base_url.to_owned()];
                page.at = 0;
                page.title = title;
            }
        }
        self.show(view, false);
    }

    fn resize(&self, view: u64, width: i32, height: i32) {
        if let Some(page) = self.lock().views.get_mut(&view) {
            page.config.width = width;
            page.config.height = height;
        }
    }

    fn set_visible(&self, _view: u64, _visible: bool) {}

    fn refresh(&self, view: u64) {
        let (w, h) = self.size(view);
        if w > 0 && h > 0 && self.lock().views.get(&view).is_some_and(|p| p.finished > 0) {
            self.queue.frame(view, &self.render(view), w, h);
        }
    }

    fn mouse(&self, _: u64, _: i32, _: i32, _: i32, _: i32, _: u32) {}

    fn wheel(&self, view: u64, _x: i32, _y: i32, _ticks_x: i32, ticks_y: i32, _mods: u32) {
        self.scroll_long(view, ticks_y);
    }

    fn key(&self, _: u64, _: bool, _: u32, _: &str, _: u32) {}

    fn focus(&self, _: u64, _: bool) {}

    fn quit(&self) {}

    fn children(&self) -> (i32, i32) {
        (0, 0)
    }

    fn load(&self, view: u64, url: &str) {
        self.navigate(view, url, true);
    }

    fn reload(&self, view: u64) {
        let url = self
            .lock()
            .views
            .get(&view)
            .and_then(|p| p.history.get(p.at).cloned());
        if let Some(url) = url {
            self.navigate(view, &url, false);
        }
    }

    fn back(&self, view: u64) {
        self.go(view, -1);
    }

    fn forward(&self, view: u64) {
        self.go(view, 1);
    }

    fn dialog_reply(&self, view: u64, dialog: u64, accepted: bool, _value: &str) {
        self.lock().replies.push((view, dialog, accepted));
        super::web_pages::dialog_answered(self, view, dialog, accepted);
    }

    fn navigation_answer(&self, view: u64, request: u64, allow: bool) {
        self.lock().answers.push((view, request, allow));
    }

    fn recover(&self, view: u64, reload: bool) {
        self.lock().recovers.push((view, reload));
    }

    fn notify_forget(&self, view: u64, origin: &str) {
        self.lock().forgets.push((view, origin.to_owned()));
    }

    fn notify_permission(&self, view: u64, origin: &str, answer: i32) {
        let mut state = self.lock();
        state.permissions.push((view, origin.to_owned(), answer));
    }

    fn blocking_compile(&self, view: u64, list: &str, json: &[u8]) {
        let ready = !json.is_empty() && json[0] == b'[';
        if ready {
            if let Some(page) = self.lock().views.get_mut(&view) {
                page.lists.push(list.to_owned());
            }
        }
        let status = match ready {
            true => wire::BLOCKING_STATUS_READY,
            false => wire::BLOCKING_STATUS_COMPILE_FAILED,
        };
        self.queue.blocking_list(view, list, status as i32);
    }
}

impl FakeWeb {
    fn go(&self, view: u64, step: isize) {
        let url = {
            let mut state = self.lock();
            let Some(page) = state.views.get_mut(&view) else {
                return;
            };
            let at = page.at as isize + step;
            if at < 0 || at as usize >= page.history.len() {
                return;
            }
            page.at = at as usize;
            page.history[page.at].clone()
        };
        self.navigate(view, &url, false);
    }

    /// The dialog step of `view` (`/alert`).
    pub(crate) fn dialog_step(&self, view: u64) -> u64 {
        self.lock().views.get(&view).map_or(0, |p| p.dialog_step)
    }
}

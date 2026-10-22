//! The full views' callbacks (`webd_ui_page_state`, `webd_ui_frame_delta`, `webd_ui_navigate`, ...):
//! on WebKit's main thread, each copies what it is told and queues it, or answers from the rules the
//! service registered. None waits for the service thread.

use super::rules::{download_key, PaintPlan};
use super::{kept, BandNow, EventQueue, Once, PageNow};
use crate::policy::Verdict;

impl EventQueue {
    /// `webd_ui_page_state`.
    pub fn page_state(&self, view: u64, state: PageNow) {
        let state = PageNow {
            url: kept(&state.url),
            title: kept(&state.title),
            error: kept(&state.error),
            committed_url: kept(&state.committed_url),
            ..state
        };
        self.lock().pages.insert(view, state);
        self.ring();
    }

    /// `webd_ui_frame_delta`: `true` when it is refused (the engine paints a whole frame next).
    pub fn frame_delta(
        &self,
        view: u64,
        rgba: &[u8],
        width: i32,
        height: i32,
        shift: Option<[i32; 6]>,
        rects: &[[i32; 4]],
    ) -> bool {
        let refused = {
            let mut inner = self.lock();
            match inner.pictures.get_mut(&view) {
                Some(pictures) => pictures.post_delta(rgba, width, height, shift, rects),
                // Not a view with deltas (or gone): nothing to apply it to.
                None => true,
            }
        };
        if !refused {
            self.ring();
        }
        refused
    }

    /// `webd_ui_band`: kept only for a view with deltas, and only as wide as the frame it goes with.
    pub fn band(&self, view: u64, doc_x: i32, doc_y: i32, width: i32, height: i32, rgba: &[u8]) {
        let bytes = (width.max(0) as usize) * (height.max(0) as usize) * 4;
        if bytes == 0 || rgba.len() < bytes || !self.lock().pictures.contains_key(&view) {
            return;
        }
        self.push_band(
            view,
            BandNow {
                doc_x,
                doc_y,
                width,
                height,
                rgba: rgba[..bytes].to_vec(),
            },
        );
    }

    pub fn paint_resize(&self, view: u64, width: i32, height: i32) {
        self.rules().paint_resize(view, width, height);
    }

    pub fn paint_full(&self, view: u64) {
        self.rules().paint_full(view);
    }

    pub fn paint_rect(&self, view: u64, rect: [i32; 4]) {
        self.rules().paint_rect(view, rect);
    }

    pub fn paint_update(&self, view: u64, area: [i32; 4], dx: i32, dy: i32, rects: &[[i32; 4]]) {
        self.rules().paint_update(view, area, dx, dy, rects);
    }

    pub fn paint_plan(&self, view: u64) -> PaintPlan {
        self.rules().paint_plan(view)
    }

    pub fn hover(&self, view: u64, url: &str) {
        self.lock().hovers.insert(view, kept(url));
        self.ring();
    }

    pub fn cursor(&self, view: u64, shape: i32) {
        self.lock().cursors.insert(view, shape);
        self.ring();
    }

    pub fn responsive(&self, view: u64, responsive: bool) {
        self.lock().responsive.insert(view, responsive);
        self.ring();
    }

    pub fn blocked(&self, view: u64, count: i32) {
        self.lock().blocked.insert(view, count);
        self.ring();
    }

    pub fn new_window(&self, view: u64, url: &str, gesture: bool) {
        if !self.rules().popups(view) {
            return;
        }
        self.push_once(Once::NewWindow {
            view,
            url: kept(url),
            gesture,
        });
    }

    /// `webd_ui_navigate`: the view's policy decides; a held navigation and a hand-off are queued for
    /// the service. An unknown view is refused.
    pub fn navigate(
        &self,
        view: u64,
        request: u64,
        url: &str,
        kind: i32,
        gesture: bool,
        main_frame: bool,
    ) -> Verdict {
        let now = self.now_ms();
        let verdict = self.rules().navigate(view, url, gesture, main_frame, now);
        match verdict {
            Verdict::Ask => self.push_once(Once::Held {
                view,
                request,
                url: kept(url),
                kind,
                gesture,
            }),
            Verdict::HandOff => self.push_once(Once::HandOff {
                view,
                url: kept(url),
            }),
            Verdict::Allow | Verdict::Deny => {}
        }
        verdict
    }

    pub fn dialog(&self, view: u64, dialog: u64, kind: i32, message: &str, default: &str) {
        self.push_once(Once::Dialog {
            view,
            dialog,
            kind,
            message: kept(message),
            default: kept(default),
        });
    }

    /// `webd_ui_download_start`: the `.part` path the engine writes, or `None` to refuse.
    pub fn download_start(
        &self,
        view: u64,
        download: u64,
        url: &str,
        suggested: &str,
        total: i64,
    ) -> Option<String> {
        let destination = self.rules().download(view, download, suggested)?;
        let part = destination.part.to_str()?.to_owned();
        let name = destination
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        self.push_once(Once::DownloadStarted {
            view,
            download,
            url: kept(url),
            name,
            total,
        });
        Some(part)
    }

    pub fn download_progress(&self, view: u64, download: u64, received: i64, total: i64) {
        {
            let mut inner = self.lock();
            // A newer count replaces the last one not yet drained: a big file must not fill the queue.
            if let Some(Once::DownloadProgress {
                view: v,
                download: d,
                received: r,
                total: t,
            }) = inner.once.back_mut()
            {
                if *v == view && *d == download {
                    (*r, *t) = (received, total);
                    return;
                }
            }
        }
        self.push_once(Once::DownloadProgress {
            view,
            download,
            received,
            total,
        });
    }

    /// Every byte arrived: the `.part` file becomes the download's file (never replacing one).
    pub fn download_done(&self, view: u64, download: u64) {
        let key = download_key(view, download);
        let Some(saver) = self.rules().saver(view) else {
            return;
        };
        let name = saver
            .destination(key)
            .and_then(|d| d.path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        self.rules().download_ended(view, download);
        match saver.finish(key, true) {
            Ok(()) => self.push_once(Once::DownloadDone {
                view,
                download,
                name,
            }),
            Err(error) => self.push_once(Once::DownloadFailed {
                view,
                download,
                reason: kept(&error.to_string()),
            }),
        }
    }

    /// The download failed or was cancelled: its `.part` file is removed.
    pub fn download_fail(&self, view: u64, download: u64, reason: &str) {
        if let Some(saver) = self.rules().saver(view) {
            let _ = saver.finish(download_key(view, download), false);
        }
        self.rules().download_ended(view, download);
        self.push_once(Once::DownloadFailed {
            view,
            download,
            reason: kept(reason),
        });
    }

    pub fn notify_show(&self, view: u64, id: u64, origin: &str, title: &str, body: &str) {
        self.notify_show_from(view, id, origin, "", title, body, "");
    }

    /// `webd_ui_notify_show` with every text the engine gives: the page that asked and its tag too.
    #[allow(clippy::too_many_arguments)]
    pub fn notify_show_from(
        &self,
        view: u64,
        id: u64,
        origin: &str,
        page_url: &str,
        title: &str,
        body: &str,
        tag: &str,
    ) {
        self.push_once(Once::NotifyShow {
            view,
            id,
            origin: kept(origin),
            page_url: kept(page_url),
            title: kept(title),
            body: kept(body),
            tag: kept(tag),
        });
    }

    pub fn notify_cancel(&self, view: u64, id: u64) {
        self.push_once(Once::NotifyCancel { view, id });
    }

    pub fn notify_gone(&self, view: u64, id: u64) {
        self.push_once(Once::NotifyGone { view, id });
    }

    pub fn notify_permission_request(&self, view: u64, origin: &str) {
        self.push_once(Once::NotifyAsk {
            view,
            origin: kept(origin),
        });
    }

    pub fn notify_permission_gone(&self, view: u64, origin: &str) {
        self.push_once(Once::NotifyAskGone {
            view,
            origin: kept(origin),
        });
    }

    /// `webd_ui_notify_permission_at`: decision `index` a page process starts with.
    pub fn notify_decision_at(&self, index: usize) -> Option<(String, bool)> {
        self.rules().notify_decision_at(index)
    }

    /// `webd_ui_blocking`: a pure decision.
    pub fn blocking(&self, view: u64, url: &str) -> bool {
        self.rules().blocking(view, url)
    }

    pub fn blocking_list(&self, view: u64, list: &str, status: i32) {
        self.push_once(Once::BlockingList {
            view,
            list: kept(list),
            status,
        });
    }
}

//! The page's questions and news (a download, dialogs, a popup, a held navigation), what a view
//! without the network scope may not do, and the end of the run.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::steps::STEP_MS;
use super::{data_config, own_config, Browse, Marker, Stage, Which};
use crate::event::Error;
use crate::full::Scope;
use crate::transport::{errno, ScopedTransport};

/// How long after the popup's request nothing else may open.
const POPUP_SETTLE_MS: u64 = 1500;
/// How long after a held navigation was answered the page must stay where it is.
const HELD_SETTLE_MS: u64 = 2000;

fn kind_name(kind: u32) -> &'static str {
    match kind {
        wire::NAVIGATION_KIND_LINK_CLICKED => "link_clicked",
        wire::NAVIGATION_KIND_FORM_SUBMITTED => "form_submitted",
        wire::NAVIGATION_KIND_BACK_FORWARD => "back_forward",
        wire::NAVIGATION_KIND_RELOAD => "reload",
        wire::NAVIGATION_KIND_FORM_RESUBMITTED => "form_resubmitted",
        wire::NAVIGATION_KIND_OTHER => "other",
        _ => "unknown",
    }
}

fn dialog_name(kind: u32) -> &'static str {
    match kind {
        wire::DIALOG_KIND_ALERT => "alert",
        wire::DIALOG_KIND_CONFIRM => "confirm",
        wire::DIALOG_KIND_PROMPT => "prompt",
        _ => "unknown",
    }
}

/// `EACCES`, as a word; anything else as what it was.
fn refused(result: &Result<(), Error>) -> String {
    match result {
        Err(Error::Refused(errno::EACCES)) => "eacces".into(),
        Err(error) => error.to_string(),
        Ok(()) => "allowed".into(),
    }
}

impl<T: ScopedTransport> Browse<T> {
    /// Start a step on view A: load `path`, or say why it could not be asked.
    fn begin_on_a(&mut self, path: &str) -> Result<(), String> {
        self.phase = 1;
        let url = self.url(path);
        self.load(Which::A, &url)
            .map_err(|error| format!("load {url}: {error}"))
    }

    pub(super) fn stage_download(&mut self, now: u64, out: &mut Vec<Marker>) {
        if self.phase == 0 {
            self.seen.download_name = None;
            self.seen.download_done = None;
            self.seen.download_failed = None;
            self.seen.download_received = 0;
            if let Err(why) = self.begin_on_a("/file.bin") {
                self.fail("DOWNLOAD", why, out);
                self.next(Stage::Dialog, now);
            }
            return;
        }
        if let Some(name) = self.seen.download_done.clone() {
            let bytes = self.seen.download_received;
            let started = self.seen.download_name.is_some();
            match started {
                true => self.pass("DOWNLOAD", format!("name={name} bytes={bytes}"), out),
                false => self.fail("DOWNLOAD", format!("done without a start: {name}"), out),
            }
        } else if let Some(reason) = self.seen.download_failed.clone() {
            self.fail("DOWNLOAD", format!("failed: {reason}"), out);
        } else if self.elapsed(now) > STEP_MS {
            let started = self.seen.download_name.is_some();
            self.fail("DOWNLOAD", format!("timeout started={started}"), out);
        } else {
            return;
        }
        self.next(Stage::Dialog, now);
    }

    pub(super) fn stage_dialog(&mut self, now: u64, out: &mut Vec<Marker>) {
        if self.phase == 0 {
            self.seen.dialogs.clear();
            if let Err(why) = self.begin_on_a("/alert") {
                self.fail("DIALOG", why, out);
                self.next(Stage::Popup, now);
            }
            return;
        }
        let title = self.title(Which::A);
        let kinds: Vec<&str> = self.seen.dialogs.iter().map(|k| dialog_name(*k)).collect();
        let detail = format!(
            "dialogs={} kinds={} title={title}",
            kinds.len(),
            kinds.join(",")
        );
        if title == "confirm=true" {
            self.pass("DIALOG", detail, out);
        } else if title.starts_with("confirm=") || self.elapsed(now) > STEP_MS {
            self.fail("DIALOG", detail, out);
        } else {
            return;
        }
        self.next(Stage::Popup, now);
    }

    pub(super) fn stage_popup(&mut self, now: u64, out: &mut Vec<Marker>) {
        match self.phase {
            0 => {
                self.seen.new_windows.clear();
                if let Err(why) = self.begin_on_a("/popup") {
                    self.fail("POPUP", why, out);
                    self.next(Stage::Held, now);
                }
            }
            1 if !self.seen.new_windows.is_empty() => {
                self.phase = 2;
                self.mark = now;
            }
            1 if self.elapsed(now) > STEP_MS => {
                self.fail("POPUP", "no NewWindowRequested".into(), out);
                self.next(Stage::Held, now);
            }
            2 if now.saturating_sub(self.mark) >= POPUP_SETTLE_MS => {
                let windows = &self.seen.new_windows;
                let (url, gesture) = windows[0].clone();
                let count = windows.len();
                let detail = format!("url={url} gesture={gesture}");
                match count == 1 && !gesture {
                    true => self.pass("POPUP", detail, out),
                    false => self.fail("POPUP", format!("count={count} {detail}"), out),
                }
                self.next(Stage::Held, now);
            }
            _ => {}
        }
    }

    pub(super) fn stage_held(&mut self, now: u64, out: &mut Vec<Marker>) {
        match self.phase {
            0 => {
                self.seen.held.clear();
                self.seen.handed_off.clear();
                if let Err(why) = self.begin_on_a("/mailto") {
                    self.fail("HELD", why, out);
                    self.next(Stage::Scope, now);
                }
            }
            1 if !self.seen.held.is_empty() => {
                self.phase = 2;
                self.mark = now;
            }
            1 if self.elapsed(now) > STEP_MS => {
                self.fail("HELD", "no NavigationRequested".into(), out);
                self.next(Stage::Scope, now);
            }
            2 if now.saturating_sub(self.mark) >= HELD_SETTLE_MS => {
                let (url, kind, gesture) = self.seen.held[0].clone();
                let committed = self.committed(Which::A);
                let detail = format!("kind={} gesture={gesture} url={url}", kind_name(kind));
                let stayed = committed == self.url("/mailto");
                let handed = self.seen.handed_off.len();
                if url.starts_with("mailto:") && stayed && handed == 0 {
                    self.pass("HELD", detail, out);
                } else {
                    let why = format!("committed={committed} handed_off={handed} {detail}");
                    self.fail("HELD", why, out);
                }
                self.next(Stage::Scope, now);
            }
            _ => {}
        }
    }

    /// `os.lazy.webd.view.v1` alone: no `Web` view, no network address, no file; a page given
    /// with `LoadHtml` loads and its picture (on the server) must not.
    pub(super) fn stage_scope(&mut self, now: u64, out: &mut Vec<Marker>) {
        if self.phase == 0 {
            self.phase = 1;
            let size = self.small_size();
            let web = self
                .make_view(Scope::View, own_config())
                .create(size.0, size.1);
            let mut data = self.make_view(Scope::View, data_config());
            if let Err(error) = data.create(size.0, size.1) {
                let why = format!("web={} data-only view: {error}", refused(&web));
                self.fail("SCOPE", why, out);
                return self.next(Stage::Delta, now);
            }
            let load = data.load(&self.url("/"));
            let file = data.load("file:///etc/passwd");
            self.other = Some(data);
            self.first_title = format!(
                "web={} load={} file={}",
                refused(&web),
                refused(&load),
                refused(&file)
            );
            self.note_page(Which::Other);
            let html = format!(
                "<!doctype html><title>No network</title><p>A page with no network.</p>\
                 <img src=\"{}\">",
                self.url("/nonet.png")
            );
            let given = match self.other.as_mut() {
                Some(view) => view.load_html(&html, "about:blank"),
                None => Err(Error::NoView),
            };
            if let Err(error) = given {
                let why = format!("{} html={error}", self.first_title);
                self.fail("SCOPE", why, out);
                self.close_other(Stage::Delta, now);
            }
            return;
        }
        if self.loaded(Which::Other, None) {
            let detail = format!("{} html=finished", self.first_title);
            match detail == "web=eacces load=eacces file=eacces html=finished" {
                true => self.pass("SCOPE", detail, out),
                false => self.fail("SCOPE", detail, out),
            }
            self.close_other(Stage::Delta, now);
        } else if self.elapsed(now) > STEP_MS {
            let why = format!("{} html=timeout", self.first_title);
            self.fail("SCOPE", why, out);
            self.close_other(Stage::Delta, now);
        }
    }

    pub(super) fn stage_done(&mut self, now: u64, out: &mut Vec<Marker>) {
        let failed: Vec<&str> = self
            .results
            .iter()
            .filter(|(_, pass)| !pass)
            .map(|(step, _)| *step)
            .collect();
        let steps = self.results.len();
        match failed.is_empty() {
            true => self.pass("DONE", format!("steps={steps}"), out),
            false => {
                let why = format!("steps={steps} failed={}", failed.join(","));
                self.fail("DONE", why, out);
            }
        }
        self.next(Stage::Idle, now);
    }
}

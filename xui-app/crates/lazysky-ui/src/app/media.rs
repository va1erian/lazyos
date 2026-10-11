//! Pictures: the viewer, Save Image As, Download all images.

use super::*;

impl LazySkyApp {
    /// The picture `index` of the post `uri` (full size and thumbnail), from
    /// whichever feed shows the post.
    fn picture_of(&self, uri: &str, index: usize) -> Option<(Post, lazysky_core::proto::ImageRef)> {
        let post = self.find_post(uri)?;
        let image = post.embed.images().get(index)?.clone();
        Some((post, image))
    }

    pub(super) fn open_viewer(&mut self, uri: &str, index: usize) {
        let Some((post, image)) = self.picture_of(uri, index) else {
            return;
        };
        let count = post.embed.images().len();
        self.viewer.state.borrow_mut().open(image.fullsize);
        self.viewer.set_title(&format!(
            "@{}  picture {} of {count}",
            post.author.handle,
            index + 1
        ));
        self.viewer.focus();
        self.viewer.invalidate();
    }

    pub(super) fn viewer_step(&mut self, ui: &mut Ui<Msg>, by: i32) {
        let Some(Nav::Viewer { uri, index }) = self.nav.last().cloned() else {
            return;
        };
        let Some((post, _)) = self.picture_of(&uri, index) else {
            return;
        };
        let count = post.embed.images().len() as i32;
        let next = (index as i32 + by).rem_euclid(count.max(1)) as usize;
        if next != index {
            self.nav.pop();
            self.push(ui, Nav::Viewer { uri, index: next });
        }
    }

    /// Asks the core for the viewer's picture at its current size and zoom
    /// (once per key).
    pub(super) fn viewer_request(&mut self) {
        if !matches!(self.nav.last(), Some(Nav::Viewer { .. })) {
            return;
        }
        let wanted = {
            let mut s = self.viewer.state.borrow_mut();
            let wanted = s.wanted();
            if wanted == s.requested {
                return;
            }
            s.requested = wanted.clone();
            s.asked_at = Some(Instant::now());
            wanted
        };
        if let Some(key) = wanted {
            let cached = self.images.borrow().contains(&key);
            (self.report)(&Msg::Report(Report::ViewerWanted(format!(
                "{}x{} cached={cached} {}",
                key.width, key.height, key.url
            ))));
            if !cached {
                self.core.want_images(vec![key]);
            }
            self.viewer.invalidate();
        }
    }

    /// Opens Save As for the picture in the viewer.
    pub(super) fn viewer_save(&mut self) {
        let Some(Nav::Viewer { uri, index }) = self.nav.last().cloned() else {
            return;
        };
        self.start_save(&uri, index);
    }

    fn start_save(&mut self, uri: &str, index: usize) {
        let Some((post, image)) = self.picture_of(uri, index) else {
            return;
        };
        let name = lazysky_core::save::file_name_for(
            &post.author.handle,
            post.rkey(),
            index + 1,
            &image.fullsize,
        );
        self.save_url = Some(image.fullsize);
        self.save_picker.set_suggested_name(&name);
        self.save_picker.open();
    }

    /// The Save As dialog accepted `path`: fetch and write it.
    pub(super) fn save_picked(&mut self, path: PathBuf) {
        let Some(url) = self.save_url.take() else {
            return;
        };
        let seq = self.seq();
        self.pending += 1;
        self.core.send(Request::SaveImage { seq, url, path });
    }

    pub(super) fn image_menu_chosen(&mut self, ui: &mut Ui<Msg>, id: MenuId) {
        let Some((uri, index)) = self.image_target.take() else {
            return;
        };
        match id {
            IMAGE_SAVE => self.start_save(&uri, index),
            IMAGE_OPEN => self.push(ui, Nav::Viewer { uri, index }),
            IMAGE_COPY => {
                if let Some((_, image)) = self.picture_of(&uri, index) {
                    ui.set_clipboard_text(&image.fullsize);
                    self.say("Image address copied.");
                }
            }
            _ => {}
        }
    }

    /// "Download all images" on the profile on show.
    pub(super) fn bulk_start(&mut self, ui: &mut Ui<Msg>) {
        let Some(Nav::Profile { actor, .. }) = self.nav.last().cloned() else {
            return;
        };
        if self.bulk_seq.is_some() {
            return;
        }
        let handle = self.profile_handle.clone().unwrap_or_else(|| actor.clone());
        let dir = lazysky_core::save::folder_for(&self.platform.pictures_dir(), &handle);
        let seq = self.seq();
        self.bulk_seq = Some(seq);
        self.pending += 1;
        self.detail.set_bulk_running(ui, true);
        self.detail.say("Scanning...");
        self.core.send(Request::BulkDownload { seq, actor, dir });
        self.set_interval(ui, drain::BUSY_MS);
    }
}

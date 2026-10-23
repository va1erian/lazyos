//! The pages of the fake web (`web::FakeWeb`): what each path of the harness server does when a
//! view opens it, as `tools/webd`'s server and WebKit would together.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::web::{FakeWeb, BASE};

/// The pixel of row `row` of the `/long` page: every row is different, so a misplaced row shows.
fn long_row(row: i32) -> [u8; 4] {
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

/// Rows of the band the `/long` page paints below the view.
pub const BAND_ROWS: i32 = 64;

/// `view` opened `path` of the server.
pub fn open(web: &FakeWeb, view: u64, path: &str) {
    let queue = web.queue();
    match path {
        "/" => {
            web.set_title(view, "Index");
            web.cookies(view, Some("webd=browser"));
            // The page has <img src=/blocked.png>; the lists apply unless this load bypasses them.
            let lists_apply = queue.blocking(view, &format!("{BASE}/"));
            if lists_apply && web.has_list(view, "test") {
                queue.blocked(view, 1);
            } else {
                web.fetch(&format!("{BASE}/blocked.png"));
            }
            web.show(view, false);
        }
        "/page2" => {
            web.set_title(view, "Page Two");
            web.show(view, false);
        }
        "/cookie" => {
            let title = format!("cookie={}", web.cookies(view, None));
            web.set_title(view, &title);
            web.show(view, false);
        }
        "/setcookie-b" => {
            web.cookies(view, Some("webd=own"));
            web.set_title(view, "Set");
            web.show(view, false);
        }
        "/alert" => {
            web.set_title(view, "Alert");
            web.show(view, true);
            web.set_dialog_step(view, 1);
            queue.dialog(view, 1, wire::DIALOG_KIND_ALERT as i32, "Hello", "");
        }
        "/popup" => {
            web.set_title(view, "Popup");
            web.show(view, false);
            queue.new_window(view, &format!("{BASE}/popped"), false);
        }
        "/mailto" => {
            web.set_title(view, "Mailto");
            web.show(view, false);
            web.page_navigates(view, "mailto:test@example.com");
        }
        "/long" => {
            web.set_title(view, "Long");
            web.show(view, false);
            let (w, h) = web.size(view);
            queue.band(view, 0, h, w, BAND_ROWS, &long_frame(w, BAND_ROWS, h));
        }
        _ => {
            web.set_title(view, "Not Found");
            web.show(view, false);
        }
    }
}

/// The app answered dialog `dialog` of `view`: `/alert` opens a confirm after its alert, then
/// sets its title to the confirm's answer.
pub fn dialog_answered(web: &FakeWeb, view: u64, dialog: u64, accepted: bool) {
    match (web.dialog_step(view), dialog) {
        (1, 1) => {
            web.set_dialog_step(view, 2);
            let kind = wire::DIALOG_KIND_CONFIRM as i32;
            web.queue().dialog(view, 2, kind, "Sure?", "");
        }
        (2, 2) => {
            web.set_dialog_step(view, 0);
            web.set_title(view, &format!("confirm={accepted}"));
            web.show(view, false);
        }
        _ => {}
    }
}

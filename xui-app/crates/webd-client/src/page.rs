//! The page the demo client (`webddemo`, `os.lazy.webddemo`) shows, and where to click on it. Kept
//! here so the app, the end-to-end tests and the QEMU harness (`tools/webd`) agree on one page:
//! a blue background (so a frame's dominant colour tells it from a blank white page), a heading, a
//! link, a form, and a picture whose address a light view must never request.

/// The picture's address. The harness listens on this port and fails the run if anything reaches it:
/// a light view has no network, so the picture must stay unloaded.
pub const BLOCKED_IMAGE_URL: &str = "http://10.0.2.2:18080/blocked.png";

/// Where the link goes. Clicking it must raise `LinkClicked` and load nothing.
pub const LINK_URL: &str = "https://example.com/clicked";

/// The page's title.
pub const TITLE: &str = "Web View Demo";

/// The link's box in view pixels `(x, y, width, height)`; the page lays it out absolutely, and the
/// app prints it as a probe rectangle (`UI:WIDGET ... name=link`) so a session clicks it by name.
pub const LINK_BOX: (i32, i32, i32, i32) = (20, 70, 260, 40);

/// The form's text field, the same way (`name=field`).
pub const FIELD_BOX: (i32, i32, i32, i32) = (20, 130, 200, 26);

/// The page, with `image_url` for its picture. The colours are pure so a frame's dominant colour
/// ([`crate::Picture::dominant`]) is blue; the link is a white box on it.
pub fn demo_page(image_url: &str) -> String {
    let (lx, ly, lw, lh) = LINK_BOX;
    let (fx, fy, fw, fh) = FIELD_BOX;
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{TITLE}</title><style>\
html,body{{margin:0;height:100%;background:#0000ff;color:#ffffff;font:18px sans-serif}}\
h1{{position:absolute;left:20px;top:10px;margin:0;font-size:32px}}\
a{{position:absolute;left:{lx}px;top:{ly}px;width:{lw}px;height:{lh}px;line-height:{lh}px;\
display:block;text-align:center;background:#ffffff;color:#0000ff;text-decoration:none}}\
form{{position:absolute;left:{fx}px;top:{fy}px;margin:0}}\
input{{width:{fw}px;height:{fh}px;box-sizing:border-box;font-size:18px}}\
img{{position:absolute;left:20px;top:190px;width:64px;height:64px}}\
</style></head><body><h1>Light view</h1>\
<a href=\"{LINK_URL}\">Follow the link</a>\
<form action=\"{LINK_URL}\"><input name=\"q\" value=\"type here\"></form>\
<img src=\"{image_url}\" alt=\"blocked\"></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_has_the_parts_the_demo_relies_on() {
        let page = demo_page(BLOCKED_IMAGE_URL);
        assert!(page.contains(&format!("<img src=\"{BLOCKED_IMAGE_URL}\"")));
        assert!(page.contains(&format!("<a href=\"{LINK_URL}\"")));
        assert!(page.contains("<form"));
        assert!(page.contains("background:#0000ff"));
        assert!(page.contains(&format!("<title>{TITLE}</title>")));
    }

    #[test]
    fn the_boxes_are_the_ones_the_style_lays_out() {
        let page = demo_page(BLOCKED_IMAGE_URL);
        assert!(page.contains("left:20px;top:70px;width:260px;height:40px"));
        assert!(page.contains("left:20px;top:130px;margin:0"));
        assert!(page.contains("width:200px;height:26px"));
    }
}

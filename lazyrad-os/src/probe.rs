//! The player's UI probe lines (issue #538, `xui_app::probe`): every named
//! control of the startup form as a `UI:WIDGET` line relative to the window's
//! content, so a session script clicks `{"click_at": {"window": "MOD Player",
//! "widget": "play_button"}}` instead of replaying measured relative moves.
//! Printed only in a `LAZYOS_UI_PROBE=1` image.
//!
//! The rectangles come from the form document (`left`/`top`/`width`/`height`,
//! a child offset by its containers), in design pixels times the backend's
//! scale; the window title is the form's `title`, which the player hands the
//! compositor as the window title.

use lazyrad_runtime::form::FormRuntime;
use xui_form::{FormDoc, Node, Value};

/// One control's rectangle in the window's content, design pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRect {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn int(node_props: Option<&Value>) -> i32 {
    match node_props {
        Some(Value::Int(n)) => i32::try_from(*n).unwrap_or(0),
        Some(Value::Float(f)) => *f as i32,
        _ => 0,
    }
}

/// The offset of `node`'s parent chain (bounded, so a cycle cannot loop).
fn parent_offset(doc: &FormDoc, node: &Node) -> (i32, i32) {
    let (mut x, mut y) = (0, 0);
    let mut parent = node.parent.as_deref();
    for _ in 0..doc.nodes.len() {
        let Some(found) = parent.and_then(|name| doc.node(name)) else {
            break;
        };
        x += int(found.prop("left"));
        y += int(found.prop("top"));
        parent = found.parent.as_deref();
    }
    (x, y)
}

/// Every node of `doc` with its rectangle in the window's content.
pub fn control_rects(doc: &FormDoc) -> Vec<ControlRect> {
    doc.nodes
        .iter()
        .map(|node| {
            let (px, py) = parent_offset(doc, node);
            ControlRect {
                name: node.name.clone(),
                x: px + int(node.prop("left")),
                y: py + int(node.prop("top")),
                w: int(node.prop("width")),
                h: int(node.prop("height")),
            }
        })
        .collect()
}

/// The window title a form gets: its `title`, else its name.
pub fn form_title(doc: &FormDoc) -> String {
    match doc.window.prop("title") {
        Some(Value::Text(title)) if !title.is_empty() => title.clone(),
        _ => doc.window.name.clone(),
    }
}

/// Print the startup form's controls when the probe is on.
pub fn print_startup_form(runtime: &FormRuntime, scale: i32) {
    if !xui_app::probe::enabled() {
        return;
    }
    let Some(form) = runtime
        .startup_name()
        .ok()
        .and_then(|name| runtime.form(&name))
    else {
        return;
    };
    let title = form_title(&form.doc);
    for rect in control_rects(&form.doc) {
        xui_app::probe::widget(
            &title,
            &rect.name,
            rect.x * scale,
            rect.y * scale,
            rect.w * scale,
            rect.h * scale,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, parent: Option<&str>, rect: [i64; 4]) -> Node {
        let mut node = Node::new("Button", name);
        node.parent = parent.map(String::from);
        for (key, value) in ["left", "top", "width", "height"].iter().zip(rect) {
            node.set_prop(*key, Value::Int(value));
        }
        node
    }

    #[test]
    fn children_are_offset_by_their_containers() {
        let mut doc = FormDoc::new("main_form");
        doc.window
            .set_prop("title", Value::Text("MOD Player".into()));
        doc.nodes = vec![
            node("panel", None, [10, 20, 200, 100]),
            node("inner", Some("panel"), [5, 5, 50, 50]),
            node("play", Some("inner"), [1, 2, 30, 20]),
        ];
        let rects = control_rects(&doc);
        assert_eq!(
            rects[2],
            ControlRect {
                name: "play".into(),
                x: 16,
                y: 27,
                w: 30,
                h: 20
            }
        );
        assert_eq!(form_title(&doc), "MOD Player");
    }

    #[test]
    fn a_parent_cycle_ends() {
        let mut doc = FormDoc::new("f");
        doc.nodes = vec![
            node("a", Some("b"), [1, 1, 1, 1]),
            node("b", Some("a"), [1, 1, 1, 1]),
        ];
        assert_eq!(control_rects(&doc).len(), 2);
        assert_eq!(form_title(&doc), "f");
    }

    #[test]
    fn the_modplayer_sample_places_its_play_button() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/samples/modplayer/main_form.lfm"
        );
        let text = std::fs::read_to_string(path).unwrap();
        let doc = FormDoc::from_toml(&text, &xui_form::Catalog::xui()).unwrap();
        let play = control_rects(&doc)
            .into_iter()
            .find(|r| r.name == "play_button")
            .unwrap();
        assert_eq!((play.x, play.y, play.w, play.h), (68, 142, 52, 28));
        assert_eq!(form_title(&doc), "MOD Player");
    }
}

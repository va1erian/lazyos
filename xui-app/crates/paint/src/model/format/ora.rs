#![forbid(unsafe_code)]

//! OpenRaster (`.ora`): a zip of a `mimetype`, a `stack.xml`, one PNG per layer
//! under `data/`, a merged image and a thumbnail.
//!
//! What round-trips: the layer order, each layer's name, opacity and
//! visibility, and which layer is selected (Krita's `selected="true"`). The
//! blend mode is Normal only in v1 (`composite-op` is read and ignored).
//! Layer offsets (`x`, `y`) are applied, so files from GIMP and Krita, which
//! store layers cropped to their content, open at full canvas size. Nested
//! stacks are flattened into the layer list, their opacity multiplied into
//! their children, their visibility and offset inherited.
//!
//! The reader treats the file as hostile: the layer count, the PNG sizes and
//! the decoded areas are bounded by the memory budget *before* anything is
//! allocated for them, and every failure is an `Err`, never a panic.

use super::xml::{self, Kind, Tag};
use super::zip::{ZipReader, ZipWriter};
use super::{HISTORY_RESERVE, png_size_ok, sniff_size};
use crate::model::bitmap::{Bitmap, MAX_SIDE};
use crate::model::image::{Document, Layer, MEMORY_BUDGET, Reserved, check_budget, required_bytes};
use crate::model::ops::{box_fit, placed};

/// The contents of the `mimetype` entry.
pub const MIMETYPE: &str = "image/openraster";
/// The longest side of the thumbnail the spec allows.
const THUMBNAIL_SIDE: u32 = 256;
/// The most layers read: a stack of more than this is not a picture.
pub const MAX_LAYERS: usize = 512;
/// The deepest nesting of stacks read.
const MAX_DEPTH: usize = 16;
/// The most elements read from `stack.xml`.
const MAX_TAGS: usize = 4 * MAX_LAYERS + 64;
/// The largest `stack.xml`.
const MAX_STACK_XML: usize = 4 * 1024 * 1024;

/// A layer as `stack.xml` describes it, before its picture is loaded.
struct RawLayer {
    name: String,
    src: String,
    opacity: f32,
    visible: bool,
    x: i32,
    y: i32,
    selected: bool,
}

/// The state inherited from the enclosing `<stack>`s.
#[derive(Clone, Copy)]
struct Group {
    opacity: f32,
    visible: bool,
    x: i32,
    y: i32,
}

fn parse_opacity(text: Option<&str>) -> f32 {
    text.and_then(|t| t.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map_or(1.0, |v| v.clamp(0.0, 1.0))
}

fn parse_offset(text: Option<&str>) -> i32 {
    text.and_then(|t| t.trim().parse::<i32>().ok()).unwrap_or(0)
}

fn is_visible(tag: &Tag) -> bool {
    tag.attr("visibility") != Some("hidden")
}

/// The canvas size and the layers (top first) described by `stack.xml`.
fn parse_stack(text: &str) -> Result<((u32, u32), Vec<RawLayer>), String> {
    let mut size = None;
    let mut groups = vec![Group {
        opacity: 1.0,
        visible: true,
        x: 0,
        y: 0,
    }];
    let mut layers = Vec::new();
    for tag in xml::tags(text, MAX_TAGS)? {
        let parent = *groups.last().expect("the root group is never popped");
        match (tag.name.as_str(), tag.kind) {
            ("image", Kind::Open | Kind::Empty) => {
                let side = |name: &str| tag.attr(name).and_then(|v| v.trim().parse::<u32>().ok());
                if let (Some(w), Some(h)) = (side("w"), side("h")) {
                    size = Some((w, h));
                }
            }
            ("stack", Kind::Open) => {
                if groups.len() > MAX_DEPTH {
                    return Err("the layer stack is nested too deeply".to_string());
                }
                groups.push(Group {
                    opacity: parent.opacity * parse_opacity(tag.attr("opacity")),
                    visible: parent.visible && is_visible(&tag),
                    x: parent.x.saturating_add(parse_offset(tag.attr("x"))),
                    y: parent.y.saturating_add(parse_offset(tag.attr("y"))),
                });
            }
            ("stack", Kind::Close) if groups.len() > 1 => {
                groups.pop();
            }
            ("layer", Kind::Open | Kind::Empty) => {
                if layers.len() >= MAX_LAYERS {
                    return Err(format!("more than {MAX_LAYERS} layers"));
                }
                layers.push(RawLayer {
                    name: tag.attr("name").unwrap_or("").to_string(),
                    src: tag.attr("src").unwrap_or("").to_string(),
                    opacity: parent.opacity * parse_opacity(tag.attr("opacity")),
                    visible: parent.visible && is_visible(&tag),
                    x: parent.x.saturating_add(parse_offset(tag.attr("x"))),
                    y: parent.y.saturating_add(parse_offset(tag.attr("y"))),
                    selected: tag.attr("selected") == Some("true"),
                });
            }
            _ => {}
        }
    }
    let (w, h) = size.ok_or("stack.xml has no image size")?;
    if !(1..=MAX_SIDE).contains(&w) || !(1..=MAX_SIDE).contains(&h) {
        return Err(format!("image size {w} x {h} is out of range"));
    }
    Ok(((w, h), layers))
}

/// Reads an OpenRaster file into a document.
pub fn read(bytes: &[u8]) -> Result<Document, String> {
    let zip = ZipReader::new(bytes)?;
    let mime = zip.read("mimetype", 256)?;
    if mime.as_deref().map(<[u8]>::trim_ascii) != Some(MIMETYPE.as_bytes()) {
        return Err("not an OpenRaster file".to_string());
    }
    let xml = zip
        .read("stack.xml", MAX_STACK_XML)?
        .ok_or("the OpenRaster file has no stack.xml")?;
    let ((width, height), raw) = parse_stack(&String::from_utf8_lossy(&xml))?;
    if raw.is_empty() {
        return Err("the OpenRaster file has no layers".to_string());
    }
    // Before any picture is decoded: the document itself must fit.
    let reserved = HISTORY_RESERVE;
    check_budget(width, height, raw.len(), reserved).map_err(|e| e.to_string())?;
    let mut layers = Vec::with_capacity(raw.len());
    let mut active = 0;
    let mut read_total = 0usize;
    // Bottom first: stack.xml lists the top layer first.
    for (n, layer) in raw.iter().rev().enumerate() {
        let data = zip
            .read(&layer.src, (MEMORY_BUDGET / 2) as usize)?
            .ok_or_else(|| format!("layer picture {:?} is missing", layer.src))?;
        read_total = read_total.saturating_add(data.len());
        if read_total as u64 > MEMORY_BUDGET {
            return Err("the layer pictures are too large".to_string());
        }
        let pixels = decode_layer(&data, (width, height), raw.len(), reserved)?;
        let canvas = if pixels.size() == (width, height) && layer.x == 0 && layer.y == 0 {
            pixels
        } else {
            placed(&pixels, width, height, [0, 0, 0, 0], layer.x, layer.y)
        };
        let mut built = Layer::new(layer.name.clone(), canvas);
        if built.name.is_empty() {
            built.name = format!("Layer {}", n + 1);
        }
        built.visible = layer.visible;
        built.opacity = (layer.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
        if layer.selected {
            active = n;
        }
        layers.push(built);
    }
    if !raw.iter().any(|l| l.selected) {
        active = layers.len() - 1;
    }
    Document::from_layers(layers, active)
}

/// Decodes one layer PNG after checking its header size against what the
/// budget leaves, since decoding allocates the whole picture (twice, briefly).
fn decode_layer(
    data: &[u8],
    canvas: (u32, u32),
    layers: usize,
    reserved: Reserved,
) -> Result<Bitmap, String> {
    let (w, h) = sniff_size(data).ok_or("a layer picture is not a PNG or JPEG")?;
    let held = required_bytes(canvas.0, canvas.1, layers).saturating_add(reserved.total());
    if !png_size_ok(w, h, held) {
        return Err(format!("a layer picture of {w} x {h} is too large"));
    }
    Bitmap::decode(data).map_err(|e| format!("a layer picture is damaged: {e}"))
}

/// Writes a document as an OpenRaster file.
pub fn write(doc: &Document) -> Result<Vec<u8>, String> {
    let layers = doc.layers();
    let selected = doc.active();
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<image w=\"{}\" h=\"{}\" version=\"0.0.3\">\n <stack opacity=\"1\" visibility=\"visible\" name=\"root\">\n",
        doc.width(),
        doc.height()
    ));
    // stack.xml lists the top layer first.
    for (i, layer) in layers.iter().enumerate().rev() {
        xml.push_str(&format!(
            "  <layer name=\"{}\" src=\"data/layer{i}.png\" opacity=\"{:.4}\" visibility=\"{}\" x=\"0\" y=\"0\" composite-op=\"svg:src-over\"{}/>\n",
            xml::escape(&layer.name),
            f32::from(layer.opacity) / 255.0,
            if layer.visible { "visible" } else { "hidden" },
            if i == selected { " selected=\"true\"" } else { "" },
        ));
    }
    xml.push_str(" </stack>\n</image>\n");

    let mut zip = ZipWriter::new();
    zip.add("mimetype", MIMETYPE.as_bytes())?;
    zip.add("stack.xml", xml.as_bytes())?;
    for (i, layer) in layers.iter().enumerate() {
        let png = layer.pixels.encode_png().map_err(|e| e.to_string())?;
        zip.add(&format!("data/layer{i}.png"), &png)?;
    }
    let merged = doc.flatten();
    zip.add(
        "mergedimage.png",
        &merged.encode_png().map_err(|e| e.to_string())?,
    )?;
    let thumb = box_fit(&merged, THUMBNAIL_SIDE);
    zip.add(
        "Thumbnails/thumbnail.png",
        &thumb.encode_png().map_err(|e| e.to_string())?,
    )?;
    zip.finish()
}

//! The `.lcur` text format (docs/cursor-editor-plan.md V1).
//!
//! ```text
//! lazyos-cursor 1
//! name midnight
//! shape hand
//! hot 224 32
//! layer fill f0f0f0 outline 000000
//! poly 176 64 200 32 232 32
//! ```
//!
//! One directive per line; blank lines and `#` comments are skipped. A file
//! is untrusted input, so [`parse`] never panics and refuses the whole file
//! at the first problem rather than guessing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::theme::{self, LayerDef, ShapeDef, Theme, DESIGN, MAX_LAYERS, MAX_VERTICES};
use crate::{Shape, SHAPE_COUNT};

const MAGIC: &str = "lazyos-cursor 1";
const NAMES: [&str; SHAPE_COUNT] = [
    "default",
    "busy",
    "text",
    "hand",
    "resize-h",
    "resize-v",
    "resize-nwse",
    "resize-nesw",
];

/// Why a file was refused: the 1-based line and what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub what: &'static str,
}

fn err<T>(line: usize, what: &'static str) -> Result<T, Error> {
    Err(Error { line, what })
}

fn shape_named(name: &str) -> Option<Shape> {
    NAMES.iter().position(|n| *n == name).map(|i| Shape::ALL[i])
}

fn colour(word: Option<&str>) -> Option<u32> {
    let word = word?;
    if word.len() != 6 || !word.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(word, 16).ok()
}

fn coord(word: &str) -> Option<i32> {
    let value: i32 = word.parse().ok()?;
    (0..=DESIGN).contains(&value).then_some(value)
}

/// A shape being read.
struct Open {
    shape: Shape,
    def: ShapeDef,
    line: usize,
}

struct Reader {
    theme: Theme,
    open: Option<Open>,
    vertices: usize,
}

impl Reader {
    fn close(&mut self) -> Result<(), Error> {
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        if !open.def.is_valid() {
            return err(open.line, "shape needs a polygon of 3+ points");
        }
        self.theme.shapes[open.shape as usize] = Some(open.def);
        Ok(())
    }

    fn directive(&mut self, n: usize, line: &str) -> Result<(), Error> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("name") => {
                let name = words.next().unwrap_or("");
                if !theme::valid_name(name) || words.next().is_some() {
                    return err(n, "bad theme name");
                }
                self.theme.name = name.into();
            }
            Some("shape") => {
                self.close()?;
                let Some(shape) = words.next().and_then(shape_named) else {
                    return err(n, "unknown shape");
                };
                if self.theme.shapes[shape as usize].is_some() {
                    return err(n, "shape given twice");
                }
                self.vertices = 0;
                let def = ShapeDef {
                    hot: (0, 0),
                    layers: Vec::new(),
                };
                self.open = Some(Open {
                    shape,
                    def,
                    line: n,
                });
            }
            Some("hot") => {
                let (Some(open), Some(x), Some(y)) = (
                    self.open.as_mut(),
                    words.next().and_then(coord),
                    words.next().and_then(coord),
                ) else {
                    return err(n, "hot needs a shape and two coordinates");
                };
                open.def.hot = (x, y);
            }
            Some("layer") => self.layer(n, words)?,
            Some("poly") => self.poly(n, words)?,
            _ => return err(n, "unknown directive"),
        }
        Ok(())
    }

    fn layer<'a>(
        &mut self,
        n: usize,
        mut words: impl Iterator<Item = &'a str>,
    ) -> Result<(), Error> {
        let Some(open) = self.open.as_mut() else {
            return err(n, "layer outside a shape");
        };
        if open.def.layers.len() >= MAX_LAYERS {
            return err(n, "too many layers");
        }
        let (mut fill, mut outline, mut thin) = (None, None, false);
        while let Some(key) = words.next() {
            match key {
                "fill" => fill = colour(words.next()),
                "outline" => outline = colour(words.next()),
                "thin" => thin = true,
                _ => return err(n, "unknown layer option"),
            }
        }
        let (Some(fill), Some(outline)) = (fill, outline) else {
            return err(n, "layer needs fill and outline as RRGGBB");
        };
        open.def.layers.push(LayerDef {
            polys: Vec::new(),
            fill,
            outline,
            thin,
        });
        Ok(())
    }

    fn poly<'a>(&mut self, n: usize, words: impl Iterator<Item = &'a str>) -> Result<(), Error> {
        let Some(layer) = self.open.as_mut().and_then(|o| o.def.layers.last_mut()) else {
            return err(n, "poly outside a layer");
        };
        let mut numbers = Vec::new();
        for word in words {
            let Some(value) = coord(word) else {
                return err(n, "coordinate outside 0..=512");
            };
            numbers.push(value);
        }
        if numbers.len() < 6 || numbers.len() % 2 != 0 {
            return err(n, "poly needs 3+ x y pairs");
        }
        self.vertices += numbers.len() / 2;
        if self.vertices > MAX_VERTICES {
            return err(n, "too many vertices");
        }
        layer
            .polys
            .push(numbers.chunks(2).map(|p| (p[0], p[1])).collect());
        Ok(())
    }
}

/// Read a theme file.
pub fn parse(text: &str) -> Result<Theme, Error> {
    let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    let header = lines.find(|(_, l)| !l.is_empty() && !l.starts_with('#'));
    if header.map(|(_, l)| l) != Some(MAGIC) {
        return err(header.map_or(1, |(n, _)| n), "not a lazyos-cursor 1 file");
    }
    let mut reader = Reader {
        theme: Theme::default(),
        open: None,
        vertices: 0,
    };
    for (n, line) in lines {
        if !line.is_empty() && !line.starts_with('#') {
            reader.directive(n, line)?;
        }
    }
    reader.close()?;
    if reader.theme.name.is_empty() {
        return err(1, "missing name");
    }
    Ok(reader.theme)
}

/// Write `theme` so that [`parse`] reads it back equal.
pub fn serialize(theme: &Theme) -> String {
    let mut out = format!("{MAGIC}\nname {}\n", theme.name);
    for (name, slot) in NAMES.iter().zip(&theme.shapes) {
        let Some(def) = slot else { continue };
        let _ = write!(out, "shape {name}\nhot {} {}\n", def.hot.0, def.hot.1);
        for layer in &def.layers {
            let thin = if layer.thin { " thin" } else { "" };
            let _ = writeln!(
                out,
                "layer fill {:06x} outline {:06x}{thin}",
                layer.fill, layer.outline
            );
            for poly in &layer.polys {
                out.push_str("poly");
                for (x, y) in poly {
                    let _ = write!(out, " {x} {y}");
                }
                out.push('\n');
            }
        }
    }
    out
}

//! A transcript row, shaped and measured at a width (docs/ai-agent-plan.md
//! §9.1): everything the painter needs and the row's final height.
//!
//! [`prepare`] runs on the layout thread (`TextShaper` and `TextLayout` are
//! `Send + Sync`) and parses the Markdown there too; the UI thread only draws
//! the result. Nothing here knows how a piece is drawn beyond its geometry.

use xui_core::backend::{FontSpec, TextLayout, TextShaper};
use xui_core::geometry::Rect;
use xui_core::Dip;

use crate::markdown::{self, Block, BlockKind};
use crate::model::{RowKind, RowSpec};

/// Design-pixel metrics (96 dpi); [`Metrics::px`] scales them.
const PAD_X: i32 = 14;
const PAD_Y: i32 = 8;
const GAP: i32 = 6;
const BODY: f32 = 14.0;
const SMALL: f32 = 11.5;
const CODE: f32 = 13.0;
const INDENT: i32 = 18;
const CODE_PAD: i32 = 8;
/// A reply is drawn at most this wide, so lines stay readable.
const MAX_TEXT: i32 = 760;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Accent,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Behind a user's message.
    User,
    /// Behind a code block.
    Code,
    /// Behind an error.
    Failure,
    /// Behind a button.
    Button,
}

/// What a click on a region of the row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A link: opened only after the user confirms the address.
    Link(String),
    /// Copy this text (a code block).
    Copy(String),
    /// Open or close a reasoning block.
    Toggle,
}

#[derive(Clone, Debug)]
pub struct Hit {
    /// Relative to the row's top-left corner.
    pub rect: Rect,
    pub target: Target,
}

pub enum Piece {
    Text {
        layout: Box<dyn TextLayout>,
        at: (i32, i32),
        tone: Tone,
    },
    Fill {
        rect: Rect,
        fill: Fill,
    },
    /// A quote's bar.
    Bar {
        rect: Rect,
    },
    /// A tint behind a link.
    LinkTint {
        rect: Rect,
    },
    Rule {
        y: i32,
    },
}

/// A row at one width.
pub struct Prepared {
    /// The row width it was laid out for, in device pixels.
    pub width: i32,
    pub height: i32,
    pub pieces: Vec<Piece>,
    pub hits: Vec<Hit>,
}

#[derive(Clone, Copy)]
pub struct Metrics {
    pub dpi: u32,
}

impl Metrics {
    pub fn px(self, design: i32) -> i32 {
        (design * self.dpi.max(1) as i32 + 48) / 96
    }
}

#[derive(Clone, Copy)]
pub struct Context {
    pub width: i32,
    pub dpi: u32,
}

fn shape(
    shaper: &dyn TextShaper,
    text: &str,
    size: f32,
    bold: bool,
    mono: bool,
    width: f32,
    dpi: u32,
) -> Box<dyn TextLayout> {
    let mut spec = FontSpec::new(Dip(size));
    if bold {
        spec = spec.weight(700);
    }
    if mono {
        spec = spec.family("monospace");
    }
    shaper.layout(text, &spec, width, dpi)
}

fn height_of(layout: &dyn TextLayout) -> i32 {
    layout.height().ceil() as i32
}

struct Page<'a> {
    shaper: &'a dyn TextShaper,
    m: Metrics,
    pieces: Vec<Piece>,
    hits: Vec<Hit>,
}

impl Page<'_> {
    fn text(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        width: i32,
        at: (i32, i32),
        tone: Tone,
    ) -> i32 {
        let layout = shape(
            self.shaper,
            text,
            size,
            bold,
            false,
            width as f32,
            self.m.dpi,
        );
        let h = height_of(&*layout);
        self.pieces.push(Piece::Text { layout, at, tone });
        h
    }

    /// A paragraph-like block: text with link tints and hit boxes.
    fn block(
        &mut self,
        block: &Block,
        size: f32,
        bold: bool,
        width: i32,
        at: (i32, i32),
        tone: Tone,
    ) -> i32 {
        let layout = shape(
            self.shaper,
            &block.text,
            size,
            bold,
            false,
            width as f32,
            self.m.dpi,
        );
        let h = height_of(&*layout);
        for link in &block.links {
            for r in layout.selection_rects(link.start, link.end) {
                let rect = Rect::new(at.0 + r.left, at.1 + r.top, at.0 + r.right, at.1 + r.bottom);
                self.pieces.push(Piece::LinkTint { rect });
                self.hits.push(Hit {
                    rect,
                    target: Target::Link(link.url.clone()),
                });
            }
        }
        self.pieces.push(Piece::Text { layout, at, tone });
        h
    }

    fn code(&mut self, lang: &str, text: &str, x: i32, y: i32, width: i32) -> i32 {
        let m = self.m;
        let pad = m.px(CODE_PAD);
        let label = if lang.is_empty() { "code" } else { lang };
        let head = shape(
            self.shaper,
            label,
            SMALL,
            false,
            false,
            f32::INFINITY,
            m.dpi,
        );
        let head_h = height_of(&*head);
        let copy = shape(
            self.shaper,
            "Copy",
            SMALL,
            true,
            false,
            f32::INFINITY,
            m.dpi,
        );
        let (copy_w, copy_h) = (copy.width().ceil() as i32, height_of(&*copy));
        let body = shape(
            self.shaper,
            text,
            CODE,
            false,
            true,
            (width - 2 * pad) as f32,
            m.dpi,
        );
        let body_h = height_of(&*body);
        let total = pad + head_h.max(copy_h) + m.px(4) + body_h + pad;
        self.pieces.push(Piece::Fill {
            rect: Rect::new(x, y, x + width, y + total),
            fill: Fill::Code,
        });
        let button = Rect::new(
            x + width - pad - copy_w - m.px(6),
            y + m.px(3),
            x + width - pad + m.px(2),
            y + pad + copy_h,
        );
        self.pieces.push(Piece::Fill {
            rect: button,
            fill: Fill::Button,
        });
        self.hits.push(Hit {
            rect: button,
            target: Target::Copy(copyable(text)),
        });
        self.pieces.push(Piece::Text {
            layout: copy,
            at: (button.left + m.px(4), y + pad - m.px(2)),
            tone: Tone::Accent,
        });
        self.pieces.push(Piece::Text {
            layout: head,
            at: (x + pad, y + pad - m.px(2)),
            tone: Tone::Secondary,
        });
        self.pieces.push(Piece::Text {
            layout: body,
            at: (x + pad, y + pad + head_h.max(copy_h) + m.px(2)),
            tone: Tone::Primary,
        });
        total
    }
}

/// The text a Copy puts on the clipboard: without the control characters
/// (an escape sequence pasted into a terminal would be run, not shown).
fn copyable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Lays `spec` out at `ctx.width`.
pub fn prepare(shaper: &dyn TextShaper, spec: &RowSpec, ctx: Context) -> Prepared {
    let m = Metrics { dpi: ctx.dpi };
    let mut page = Page {
        shaper,
        m,
        pieces: Vec::new(),
        hits: Vec::new(),
    };
    let pad_x = m.px(PAD_X);
    let x0 = pad_x;
    let width = (ctx.width - 2 * pad_x).clamp(m.px(120), m.px(MAX_TEXT));
    let mut y = m.px(PAD_Y);
    match spec.kind {
        RowKind::User => {
            let top = y;
            let inner = width - 2 * m.px(8);
            let label_h = page.text(
                "You",
                SMALL,
                true,
                inner,
                (x0 + m.px(8), y + m.px(6)),
                Tone::Secondary,
            );
            let body_y = y + m.px(6) + label_h + m.px(2);
            let body_h = page.text(
                &spec.text,
                BODY,
                false,
                inner,
                (x0 + m.px(8), body_y),
                Tone::Primary,
            );
            let bottom = body_y + body_h + m.px(8);
            page.pieces.insert(
                0,
                Piece::Fill {
                    rect: Rect::new(x0, top, x0 + width, bottom),
                    fill: Fill::User,
                },
            );
            y = bottom + m.px(PAD_Y);
        }
        RowKind::Assistant => {
            let label_h = page.text("Assistant", SMALL, true, width, (x0, y), Tone::Secondary);
            y += label_h + m.px(2);
            for block in markdown::parse(&spec.text) {
                y += assistant_block(&mut page, &block, x0, y, width) + m.px(GAP);
            }
            if spec.text.trim().is_empty() {
                y += page.text("...", BODY, false, width, (x0, y), Tone::Secondary);
            }
            y += m.px(PAD_Y);
        }
        RowKind::Thinking { open } => {
            let head = if open {
                "Thinking (click to hide)".to_string()
            } else {
                format!(
                    "Thinking... {} characters (click to show)",
                    spec.text.chars().count()
                )
            };
            let h = page.text(&head, SMALL, false, width, (x0, y), Tone::Secondary);
            page.hits.push(Hit {
                rect: Rect::new(x0, y, x0 + width, y + h),
                target: Target::Toggle,
            });
            y += h;
            if open {
                y += m.px(4);
                let body = shape(
                    page.shaper,
                    &spec.text,
                    BODY - 1.0,
                    false,
                    false,
                    (width - m.px(INDENT)) as f32,
                    m.dpi,
                );
                let h = height_of(&*body);
                page.pieces.push(Piece::Bar {
                    rect: Rect::new(x0 + m.px(4), y, x0 + m.px(7), y + h),
                });
                page.pieces.push(Piece::Text {
                    layout: body,
                    at: (x0 + m.px(INDENT), y),
                    tone: Tone::Secondary,
                });
                y += h;
            }
            y += m.px(PAD_Y);
        }
        RowKind::Failure => {
            let top = y;
            let inner = width - 2 * m.px(8);
            let h = page.text(
                &spec.text,
                BODY - 1.0,
                false,
                inner,
                (x0 + m.px(8), y + m.px(6)),
                Tone::Danger,
            );
            let bottom = top + h + m.px(12);
            page.pieces.insert(
                0,
                Piece::Fill {
                    rect: Rect::new(x0, top, x0 + width, bottom),
                    fill: Fill::Failure,
                },
            );
            y = bottom + m.px(PAD_Y);
        }
        RowKind::Notice => {
            let h = page.text(&spec.text, SMALL, false, width, (x0, y), Tone::Secondary);
            y += h + m.px(PAD_Y);
        }
    }
    Prepared {
        width: ctx.width,
        height: y.max(1),
        pieces: page.pieces,
        hits: page.hits,
    }
}

fn assistant_block(page: &mut Page<'_>, block: &Block, x: i32, y: i32, width: i32) -> i32 {
    let m = page.m;
    match &block.kind {
        BlockKind::Paragraph => page.block(block, BODY, false, width, (x, y), Tone::Primary),
        BlockKind::Heading(level) => {
            let size = match level {
                1 => 19.0,
                2 => 16.5,
                _ => 15.0,
            };
            page.block(block, size, true, width, (x, y), Tone::Primary)
        }
        BlockKind::Item { marker, depth } => {
            let indent = m.px(INDENT) * (i32::from(*depth) + 1);
            let marker_w = m.px(20);
            page.text(
                marker,
                BODY,
                false,
                marker_w,
                (x + indent - marker_w, y),
                Tone::Secondary,
            );
            page.block(
                block,
                BODY,
                false,
                width - indent,
                (x + indent, y),
                Tone::Primary,
            )
        }
        BlockKind::Quote => {
            let inset = m.px(14);
            let h = page.block(
                block,
                BODY,
                false,
                width - inset,
                (x + inset, y),
                Tone::Secondary,
            );
            page.pieces.push(Piece::Bar {
                rect: Rect::new(x + m.px(3), y, x + m.px(6), y + h),
            });
            h
        }
        BlockKind::Code(lang) => page.code(lang, &block.text, x, y, width),
        BlockKind::Rule => {
            page.pieces.push(Piece::Rule { y: y + m.px(3) });
            m.px(6)
        }
    }
}

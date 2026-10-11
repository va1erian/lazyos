//! A feed row, shaped and measured at a width (docs/lazysky-plan.md §6.1,
//! §6.2, R2): everything the painter needs, and the row's final height.
//!
//! [`prepare`] runs on the layout thread (`TextShaper` and `TextLayout` are
//! `Send + Sync`); the UI thread only draws the result. Nothing here knows
//! how a row is drawn beyond its geometry: the painter (`feedview/paint.rs`)
//! turns [`Piece`]s into pixels.

use lazysky_core::proto::embed::{Embed, ImageRef, Quoted};
use lazysky_core::proto::facets::Feature;
use lazysky_core::proto::time::{parse_rfc3339, short_age};
use lazysky_core::proto::{FeedItem, Reason};
use xui_core::backend::{FontSpec, TextLayout, TextShaper};
use xui_core::geometry::Rect;
use xui_core::Dip;

/// Design-pixel metrics (96 dpi); [`Metrics::px`] scales them.
const PAD_X: i32 = 12;
const PAD_Y: i32 = 10;
const AVATAR: i32 = 48;
const GAP: i32 = 10;
const ACTION_GAP: i32 = 22;
const QUOTE_PAD: i32 = 8;
/// The text size classes.
const SIZE_BODY: f32 = 14.0;
const SIZE_SMALL: f32 = 12.0;
/// The tallest a single full-width image gets (the settings page overrides).
pub const DEFAULT_MAX_IMAGE_H: i32 = 420;
/// A quote's text is cut to this many characters (no layout line limit).
const QUOTE_CHARS: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Accent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Reply,
    Repost,
    Like,
    More,
}

/// What a click on a region of the row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Link(String),
    Mention(String),
    Tag(String),
    /// The `n`th picture of the post.
    Image(usize),
    Action(Action),
    /// The "Load more" row.
    LoadMore,
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
    /// The box a picture will fill (P4): drawn grey until it does.
    Media { rect: Rect, label: &'static str },
    /// The author's avatar circle.
    Avatar { rect: Rect },
    /// A quote or link-card frame.
    Frame { rect: Rect },
    /// A tint behind a link or mention.
    LinkTint { rect: Rect },
    /// The row's bottom rule.
    Rule { y: i32 },
}

/// A row at one width.
pub struct Prepared {
    /// The row width it was laid out for, in device pixels.
    pub width: i32,
    pub height: i32,
    pub pieces: Vec<Piece>,
    pub hits: Vec<Hit>,
}

/// Scale from design pixels.
#[derive(Clone, Copy)]
pub struct Metrics {
    pub dpi: u32,
}

impl Metrics {
    pub fn px(self, design: i32) -> i32 {
        (design * self.dpi.max(1) as i32 + 48) / 96
    }
}

/// What `prepare` needs besides the post.
#[derive(Clone, Copy)]
pub struct Context {
    pub width: i32,
    pub dpi: u32,
    /// Unix seconds, for ages ("3h").
    pub now: u64,
    /// The tallest a lone image box gets, in design pixels.
    pub max_image_h: i32,
}

fn shape(
    shaper: &dyn TextShaper,
    text: &str,
    size: f32,
    bold: bool,
    width: f32,
    dpi: u32,
) -> Box<dyn TextLayout> {
    let mut spec = FontSpec::new(Dip(size));
    if bold {
        spec = spec.weight(700);
    }
    shaper.layout(text, &spec, width, dpi)
}

fn height_of(layout: &dyn TextLayout) -> i32 {
    layout.height().ceil() as i32
}

/// Lays `item` out at `ctx.width`.
pub fn prepare(shaper: &dyn TextShaper, item: &FeedItem, ctx: Context) -> Prepared {
    let m = Metrics { dpi: ctx.dpi };
    let post = &item.post;
    let (pad_x, pad_y) = (m.px(PAD_X), m.px(PAD_Y));
    let x0 = pad_x + m.px(AVATAR) + m.px(GAP);
    let text_w = (ctx.width - x0 - pad_x).max(m.px(80));
    let mut pieces = Vec::new();
    let mut hits = Vec::new();
    let mut y = pad_y;

    if let Some(Reason::Repost { by }) = &item.reason {
        let line = shape(
            shaper,
            &format!("Reposted by {}", by.name()),
            SIZE_SMALL,
            false,
            text_w as f32,
            ctx.dpi,
        );
        let h = height_of(&*line);
        pieces.push(Piece::Text {
            layout: line,
            at: (x0, y),
            tone: Tone::Secondary,
        });
        y += h + m.px(2);
    }

    let avatar_bottom = y + m.px(AVATAR);
    pieces.push(Piece::Avatar {
        rect: Rect::new(pad_x, y, pad_x + m.px(AVATAR), y + m.px(AVATAR)),
    });
    hits.push(Hit {
        rect: Rect::new(pad_x, y, pad_x + m.px(AVATAR), y + m.px(AVATAR)),
        target: Target::Mention(post.author.did.clone()),
    });

    let name = shape(
        shaper,
        post.author.name(),
        SIZE_BODY,
        true,
        text_w as f32,
        ctx.dpi,
    );
    let name_h = height_of(&*name);
    pieces.push(Piece::Text {
        layout: name,
        at: (x0, y),
        tone: Tone::Primary,
    });
    y += name_h;
    let age = parse_rfc3339(&post.created_at)
        .or_else(|| parse_rfc3339(&post.indexed_at))
        .map(|then| short_age(ctx.now, then));
    let meta_text = match age {
        Some(age) => format!("@{} \u{b7} {age}", post.author.handle),
        None => format!("@{}", post.author.handle),
    };
    let meta = shape(
        shaper,
        &meta_text,
        SIZE_SMALL,
        false,
        text_w as f32,
        ctx.dpi,
    );
    let meta_h = height_of(&*meta);
    pieces.push(Piece::Text {
        layout: meta,
        at: (x0, y),
        tone: Tone::Secondary,
    });
    y += meta_h + m.px(4);

    if !post.text.is_empty() {
        let body = shape(shaper, &post.text, SIZE_BODY, false, text_w as f32, ctx.dpi);
        let body_h = height_of(&*body);
        link_hits(&*body, post, (x0, y), &mut pieces, &mut hits);
        pieces.push(Piece::Text {
            layout: body,
            at: (x0, y),
            tone: Tone::Primary,
        });
        y += body_h + m.px(6);
    }

    y = embed_box(
        shaper,
        &post.embed,
        ctx,
        m,
        (x0, y, text_w),
        &mut pieces,
        &mut hits,
    );

    // The avatar column is at least as tall as the row's text above the
    // action line.
    y = y.max(avatar_bottom + m.px(6));

    // The action line: Reply, Repost, Like with their counts, and "...".
    let actions = [
        (Action::Reply, format!("Reply {}", post.reply_count)),
        (Action::Repost, format!("Repost {}", post.repost_count)),
        (Action::Like, format!("Like {}", post.like_count)),
        (Action::More, "...".to_string()),
    ];
    let mut x = x0;
    let mut line_h = 0;
    for (action, text) in actions {
        let tone = match action {
            Action::Like if post.viewer.like.is_some() => Tone::Accent,
            Action::Repost if post.viewer.repost.is_some() => Tone::Accent,
            _ => Tone::Secondary,
        };
        let layout = shape(shaper, &text, SIZE_SMALL, false, f32::INFINITY, ctx.dpi);
        let (w, h) = (layout.width().ceil() as i32, height_of(&*layout));
        hits.push(Hit {
            rect: Rect::new(x, y, x + w, y + h),
            target: Target::Action(action),
        });
        pieces.push(Piece::Text {
            layout,
            at: (x, y),
            tone,
        });
        x += w + m.px(ACTION_GAP);
        line_h = line_h.max(h);
    }
    y += line_h + pad_y;
    pieces.push(Piece::Rule { y: y - 1 });
    Prepared {
        width: ctx.width,
        height: y,
        pieces,
        hits,
    }
}

/// Hit boxes and tints for the post's links and mentions.
fn link_hits(
    body: &dyn TextLayout,
    post: &lazysky_core::proto::Post,
    origin: (i32, i32),
    pieces: &mut Vec<Piece>,
    hits: &mut Vec<Hit>,
) {
    for facet in &post.facets {
        let target = match &facet.feature {
            Feature::Link(uri) => Target::Link(uri.clone()),
            Feature::Mention(did) => Target::Mention(did.clone()),
            Feature::Tag(tag) => Target::Tag(tag.clone()),
        };
        for r in body.selection_rects(facet.start, facet.end) {
            let rect = Rect::new(
                origin.0 + r.left,
                origin.1 + r.top,
                origin.0 + r.right,
                origin.1 + r.bottom,
            );
            pieces.push(Piece::LinkTint { rect });
            hits.push(Hit {
                rect,
                target: target.clone(),
            });
        }
    }
}

/// Lays out the embed under the text at `(x, y)` in a column `w` wide;
/// returns the new `y`.
fn embed_box(
    shaper: &dyn TextShaper,
    media: &Embed,
    ctx: Context,
    m: Metrics,
    (x, y, w): (i32, i32, i32),
    pieces: &mut Vec<Piece>,
    hits: &mut Vec<Hit>,
) -> i32 {
    match media {
        Embed::Images(images) => images_box(images, ctx, m, (x, y, w), pieces, hits),
        Embed::Video(video) => {
            let (aw, ah) = video.aspect.unwrap_or((16, 9));
            let h = (w as i64 * ah as i64 / aw.max(1) as i64) as i32;
            let h = h.clamp(m.px(60), m.px(ctx.max_image_h));
            pieces.push(Piece::Media {
                rect: Rect::new(x, y, x + w, y + h),
                label: "Video",
            });
            y + h + m.px(6)
        }
        Embed::External(card) => {
            let pad = m.px(QUOTE_PAD);
            let inner = (w - 2 * pad).max(1) as f32;
            let title = shape(shaper, &card.title, SIZE_BODY, true, inner, ctx.dpi);
            let host = shape(
                shaper,
                &host_of(&card.uri),
                SIZE_SMALL,
                false,
                inner,
                ctx.dpi,
            );
            let (th, hh) = (height_of(&*title), height_of(&*host));
            let rect = Rect::new(x, y, x + w, y + pad + th + hh + pad);
            pieces.push(Piece::Frame { rect });
            pieces.push(Piece::Text {
                layout: title,
                at: (x + pad, y + pad),
                tone: Tone::Primary,
            });
            pieces.push(Piece::Text {
                layout: host,
                at: (x + pad, y + pad + th),
                tone: Tone::Secondary,
            });
            hits.push(Hit {
                rect,
                target: Target::Link(card.uri.clone()),
            });
            rect.bottom + m.px(6)
        }
        Embed::Record(quoted) => quote(shaper, quoted, None, ctx, m, (x, y, w), pieces),
        Embed::RecordWithMedia(quoted, inner) => {
            let below = embed_box(shaper, inner, ctx, m, (x, y, w), pieces, hits);
            quote(shaper, quoted, Some(below), ctx, m, (x, y, w), pieces)
        }
        Embed::None | Embed::Unknown => y,
    }
}

fn host_of(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

fn images_box(
    images: &[ImageRef],
    ctx: Context,
    m: Metrics,
    (x, y, w): (i32, i32, i32),
    pieces: &mut Vec<Piece>,
    hits: &mut Vec<Hit>,
) -> i32 {
    if images.is_empty() {
        return y;
    }
    let gap = m.px(4);
    let max_h = m.px(ctx.max_image_h);
    let ratio = |image: &ImageRef| {
        let (aw, ah) = image.aspect_or_default();
        ah as f32 / aw.max(1) as f32
    };
    let mut rects = Vec::new();
    let bottom = if images.len() == 1 {
        let h = ((w as f32 * ratio(&images[0])) as i32).clamp(m.px(60), max_h);
        rects.push(Rect::new(x, y, x + w, y + h));
        y + h
    } else {
        let col = (w - gap) / 2;
        // A row of two is as tall as its first picture's ratio asks, within
        // sane bounds; further rows repeat it.
        let h = ((col as f32 * ratio(&images[0]).clamp(0.5, 1.2)) as i32).min(max_h / 2 + m.px(40));
        for (i, _) in images.iter().enumerate().take(4) {
            let (cx, cy) = ((i % 2) as i32, (i / 2) as i32);
            let left = x + cx * (col + gap);
            let top = y + cy * (h + gap);
            rects.push(Rect::new(left, top, left + col, top + h));
        }
        y + ((images.len().min(4) as i32 + 1) / 2) * h
            + ((images.len().min(4) as i32 - 1) / 2) * gap
    };
    for (i, rect) in rects.into_iter().enumerate() {
        pieces.push(Piece::Media {
            rect,
            label: "Picture",
        });
        hits.push(Hit {
            rect,
            target: Target::Image(i),
        });
    }
    bottom + m.px(6)
}

/// A quoted post in a frame: its author and its (shortened) text. With
/// `top`, the frame sits there (below the media of a quote-with-media).
fn quote(
    shaper: &dyn TextShaper,
    quoted: &Quoted,
    top: Option<i32>,
    ctx: Context,
    m: Metrics,
    (x, y, w): (i32, i32, i32),
    pieces: &mut Vec<Piece>,
) -> i32 {
    let y = top.unwrap_or(y);
    let pad = m.px(QUOTE_PAD);
    let inner = (w - 2 * pad).max(1) as f32;
    let (head, body) = match quoted {
        Quoted::Post(q) => {
            let mut text: String = q.text.chars().take(QUOTE_CHARS).collect();
            if q.text.chars().count() > QUOTE_CHARS {
                text.push_str("...");
            }
            (format!("{} @{}", q.author.name(), q.author.handle), text)
        }
        Quoted::NotFound(_) => ("Quoted post".to_string(), "This post was deleted.".into()),
        Quoted::Blocked(_) => ("Quoted post".to_string(), "This post is blocked.".into()),
        Quoted::Detached(_) => ("Quoted post".to_string(), "The quote was removed.".into()),
        Quoted::Unknown => (
            "Quoted post".to_string(),
            "This post cannot be shown.".into(),
        ),
    };
    let head = shape(shaper, &head, SIZE_SMALL, true, inner, ctx.dpi);
    let head_h = height_of(&*head);
    let body = (!body.is_empty()).then(|| shape(shaper, &body, SIZE_BODY, false, inner, ctx.dpi));
    let body_h = body.as_deref().map_or(0, height_of);
    let rect = Rect::new(x, y, x + w, y + pad + head_h + body_h + pad);
    pieces.push(Piece::Frame { rect });
    pieces.push(Piece::Text {
        layout: head,
        at: (x + pad, y + pad),
        tone: Tone::Secondary,
    });
    if let Some(body) = body {
        pieces.push(Piece::Text {
            layout: body,
            at: (x + pad, y + pad + head_h),
            tone: Tone::Primary,
        });
    }
    rect.bottom + m.px(6)
}

/// The "Load more" row: one centred line.
pub fn prepare_load_more(shaper: &dyn TextShaper, ctx: Context) -> Prepared {
    let m = Metrics { dpi: ctx.dpi };
    let label = shape(shaper, "Load more", SIZE_BODY, true, f32::INFINITY, ctx.dpi);
    let (w, h) = (label.width().ceil() as i32, height_of(&*label));
    let height = h + 2 * m.px(PAD_Y) + m.px(16);
    let at = ((ctx.width - w) / 2, (height - h) / 2);
    let rect = Rect::new(0, 0, ctx.width, height);
    Prepared {
        width: ctx.width,
        height,
        pieces: vec![Piece::Text {
            layout: label,
            at,
            tone: Tone::Accent,
        }],
        hits: vec![Hit {
            rect,
            target: Target::LoadMore,
        }],
    }
}

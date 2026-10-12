//! Embeds under a post's text: picture grids, link cards, videos and
//! quotes. Geometry only: the painter draws it.

use lazysky_core::images::Shape;
use lazysky_core::proto::embed::{Embed, ImageRef, Quoted};
use xui_core::backend::TextShaper;
use xui_core::geometry::Rect;

use super::{
    height_of, picture_key, shape, Context, Hit, Metrics, Piece, Target, Tone, QUOTE_CHARS,
    QUOTE_PAD, SIZE_BODY, SIZE_SMALL,
};

/// Lays out the embed under the text at `(x, y)` in a column `w` wide;
/// returns the new `y`.
pub(super) fn embed_box(
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
            let rect = Rect::new(x, y, x + w, y + h);
            pieces.push(Piece::Media {
                rect,
                label: "Video",
                key: video
                    .thumbnail
                    .as_ref()
                    .map(|url| picture_key(url, rect.width(), rect.height(), Shape::Cover)),
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
    for (i, (rect, image)) in rects.into_iter().zip(images).enumerate() {
        pieces.push(Piece::Media {
            rect,
            label: "Picture",
            key: Some(picture_key(
                &image.thumb,
                rect.width(),
                rect.height(),
                Shape::Cover,
            )),
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

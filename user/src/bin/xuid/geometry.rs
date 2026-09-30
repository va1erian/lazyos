//! Pure window geometry: frame edge hit-testing, resize clamping,
//! the off-screen reachability rule, the maximized rectangle and size-hint
//! validation. None of it touches compositor state, so the boot self-test can
//! exercise every rule directly.

use user::messenger::display::Rect;

use super::theme::{
    BORDER, BUTTON, BUTTON_GAP, BUTTON_MARGIN, CORNER_GRIP, MIN_CONTENT_H, MIN_CONTENT_W,
    RESIZE_GRIP, RESIZE_OUT, TITLE_H, TITLE_REACHABLE_W,
};

/// Which edges of a window frame a resize drag has grabbed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Edges(u8);

impl Edges {
    /// No edge grabbed.
    pub(super) const EMPTY: Edges = Edges(0);
    /// The left side.
    const LEFT: u8 = 1;
    /// The right side.
    const RIGHT: u8 = 2;
    /// The top side.
    const TOP: u8 = 4;
    /// The bottom side.
    const BOTTOM: u8 = 8;

    /// Whether no edge is grabbed.
    pub(super) fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `edge` (one of the private bits) is grabbed.
    fn has(self, edge: u8) -> bool {
        self.0 & edge != 0
    }
}

/// The content-size bounds a client declared with `SetSizeHints`, already
/// clamped to the compositor's minimums and the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct SizeHints {
    /// Minimum content width in pixels.
    pub(super) min_w: i32,
    /// Minimum content height in pixels.
    pub(super) min_h: i32,
    /// Maximum content width in pixels.
    pub(super) max_w: i32,
    /// Maximum content height in pixels.
    pub(super) max_h: i32,
}

impl SizeHints {
    /// Validate and clamp a `SetSizeHints` request against `screen`, or `None`
    /// when the resulting minimum exceeds the maximum (`EINVAL`). A `max_*` of
    /// 0 means the largest content size the screen allows.
    pub(super) fn new(
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
        screen: (i32, i32),
    ) -> Option<SizeHints> {
        let screen_max_w = (screen.0 - BORDER * 2).max(MIN_CONTENT_W);
        let screen_max_h = (screen.1 - TITLE_H - BORDER).max(MIN_CONTENT_H);
        let min_w = i32::try_from(min_w).unwrap_or(i32::MAX).max(MIN_CONTENT_W);
        let min_h = i32::try_from(min_h).unwrap_or(i32::MAX).max(MIN_CONTENT_H);
        let max_w = if max_w == 0 {
            screen_max_w
        } else {
            i32::try_from(max_w).unwrap_or(i32::MAX)
        };
        let max_h = if max_h == 0 {
            screen_max_h
        } else {
            i32::try_from(max_h).unwrap_or(i32::MAX)
        };
        if min_w > max_w || min_h > max_h || min_w > screen_max_w || min_h > screen_max_h {
            return None;
        }
        Some(SizeHints {
            min_w,
            min_h,
            max_w: max_w.min(screen_max_w),
            max_h: max_h.min(screen_max_h),
        })
    }
}

/// Whether `point` lies inside `rect` (half-open: right/bottom exclusive).
fn contains(rect: Rect, point: (i32, i32)) -> bool {
    point.0 >= rect.x && point.1 >= rect.y && point.0 < rect.x + rect.w && point.1 < rect.y + rect.h
}

/// `rect` grown by `by` pixels on every side.
pub(super) fn inflate(rect: Rect, by: i32) -> Rect {
    Rect::new(rect.x - by, rect.y - by, rect.w + by * 2, rect.h + by * 2)
}

/// The title-bar button group (all three buttons and their margins), excluded
/// from edge hits so a button click never starts a resize.
fn buttons(window: Rect) -> Rect {
    let group_w = BUTTON * 3 + BUTTON_GAP * 2;
    Rect::new(
        window.x + window.w - BUTTON_MARGIN - group_w,
        window.y,
        group_w + BUTTON_MARGIN,
        TITLE_H,
    )
}

/// Which frame edges `point` grabs on `window`, or [`Edges::EMPTY`] when it is
/// not on the frame. The grip is [`RESIZE_GRIP`] wide (2 px outside, 4 px
/// inside); a corner widens it to [`CORNER_GRIP`] so diagonal resize is easy.
/// The title bar's body and the buttons stay move/click handles.
pub(super) fn hit_edges(window: Rect, point: (i32, i32)) -> Edges {
    let (x, y) = (window.x, window.y);
    let (right, bottom) = (x + window.w, y + window.h);
    let outer = Rect::new(
        x - RESIZE_OUT,
        y - RESIZE_OUT,
        window.w + RESIZE_OUT * 2,
        window.h + RESIZE_OUT * 2,
    );
    if !contains(outer, point) || contains(buttons(window), point) {
        return Edges::EMPTY;
    }
    let near_left = point.0 >= x - RESIZE_OUT && point.0 < x + RESIZE_GRIP;
    let near_right = point.0 > right - RESIZE_GRIP - 1 && point.0 <= right + RESIZE_OUT;
    let near_top = point.1 >= y - RESIZE_OUT && point.1 < y + RESIZE_GRIP;
    let near_bottom = point.1 > bottom - RESIZE_GRIP - 1 && point.1 <= bottom + RESIZE_OUT;
    let in_left_corner = point.0 >= x - RESIZE_OUT && point.0 < x + CORNER_GRIP;
    let in_right_corner = point.0 > right - CORNER_GRIP && point.0 <= right + RESIZE_OUT;
    let in_top_corner = point.1 >= y - RESIZE_OUT && point.1 < y + CORNER_GRIP;
    let in_bottom_corner = point.1 > bottom - CORNER_GRIP && point.1 <= bottom + RESIZE_OUT;

    let mut edges = Edges::EMPTY;
    if near_left || (in_left_corner && (in_top_corner || in_bottom_corner)) {
        edges.0 |= Edges::LEFT;
    }
    if near_right || (in_right_corner && (in_top_corner || in_bottom_corner)) {
        edges.0 |= Edges::RIGHT;
    }
    if near_top || (in_top_corner && (in_left_corner || in_right_corner)) {
        edges.0 |= Edges::TOP;
    }
    if near_bottom || (in_bottom_corner && (in_left_corner || in_right_corner)) {
        edges.0 |= Edges::BOTTOM;
    }
    edges
}

/// The window rectangle after dragging `edges` by `(dx, dy)` and clamping the
/// content size to `[min, max]` (content pixels), keeping the title bar
/// reachable on `work`. The edge opposite a clamp stays fixed.
pub(super) fn resize_rect(
    start: Rect,
    edges: Edges,
    (dx, dy): (i32, i32),
    (min, max): ((i32, i32), (i32, i32)),
    work: Rect,
) -> Rect {
    let (mut left, mut top) = (start.x, start.y);
    let (mut right, mut bottom) = (start.x + start.w, start.y + start.h);
    if edges.has(Edges::LEFT) {
        left = left.saturating_add(dx);
    }
    if edges.has(Edges::RIGHT) {
        right = right.saturating_add(dx);
    }
    if edges.has(Edges::TOP) {
        top = top.saturating_add(dy);
    }
    if edges.has(Edges::BOTTOM) {
        bottom = bottom.saturating_add(dy);
    }
    let min_w = min.0.max(0) + BORDER * 2;
    let min_h = min.1.max(0) + TITLE_H + BORDER;
    let max_w = max.0.max(min.0) + BORDER * 2;
    let max_h = max.1.max(min.1) + TITLE_H + BORDER;
    // Each moving edge is bounded by the size limits against its anchored
    // opposite edge and by the rule `keep_reachable` enforces for moves: part
    // of the title bar stays on the work area. The anchor never moves.
    let (work_right, work_bottom) = (work.x + work.w, work.y + work.h);
    if edges.has(Edges::LEFT) {
        let lo = right - max_w;
        let hi = (right - min_w).min(work_right - TITLE_REACHABLE_W);
        left = clamp_edge(left, lo, hi, start.x);
    } else if edges.has(Edges::RIGHT) {
        let lo = (left + min_w).max(work.x + TITLE_REACHABLE_W);
        right = clamp_edge(right, lo, left + max_w, start.x + start.w);
    }
    if edges.has(Edges::TOP) {
        let lo = (bottom - max_h).max(work.y);
        let hi = (bottom - min_h).min(work_bottom - TITLE_H);
        top = clamp_edge(top, lo, hi, start.y);
    } else if edges.has(Edges::BOTTOM) {
        // The bottom edge may leave the screen: the title bar is at the top.
        bottom = bottom.min(top + max_h).max(top + min_h);
    }
    Rect::new(left, top, right - left, bottom - top)
}

/// `value` clamped to `[lo, hi]`, or `fallback` (the edge's starting position)
/// when the size limits and the reachability rule leave no valid position.
fn clamp_edge(value: i32, lo: i32, hi: i32, fallback: i32) -> i32 {
    if lo <= hi {
        value.clamp(lo, hi)
    } else {
        fallback
    }
}

/// Clamp a dragged window origin so at least [`TITLE_REACHABLE_W`] pixels of
/// its title bar stay inside `work` horizontally, and the title bar never goes
/// above the work-area top or below its bottom. The body may hang off the
/// left, right and bottom edges.
pub(super) fn keep_reachable(window: Rect, work: Rect) -> (i32, i32) {
    let lo_x = work.x - window.w + TITLE_REACHABLE_W;
    let hi_x = work.x + work.w - TITLE_REACHABLE_W;
    let lo_y = work.y;
    let hi_y = work.y + work.h - TITLE_H;
    (window.x.max(lo_x).min(hi_x), window.y.max(lo_y).min(hi_y))
}

/// The rectangle a window fills when maximized: the whole work area.
pub(super) fn maximized_rect(work: Rect) -> Rect {
    work
}

/// Shift `rect` (keeping its size) so it lies inside `bounds` when it fits.
pub(super) fn clamp_into(rect: Rect, bounds: Rect) -> Rect {
    let x = rect
        .x
        .max(bounds.x)
        .min((bounds.x + bounds.w - rect.w).max(bounds.x));
    let y = rect
        .y
        .max(bounds.y)
        .min((bounds.y + bounds.h - rect.h).max(bounds.y));
    Rect::new(x, y, rect.w, rect.h)
}

/// Boot check of the pure geometry rules: `XUID:GEOM:PASS` or
/// `XUID:GEOM:FAIL`.
pub(super) fn selftest_geometry() -> &'static str {
    // A 200x150 window at (100, 50); content 196x126.
    let window = Rect::new(100, 50, 200, 150);
    // The left border and the two corners are edges.
    let left = hit_edges(window, (99, 120)) == Edges(Edges::LEFT);
    let bottom_right = hit_edges(window, (299, 199)) == Edges(Edges::RIGHT | Edges::BOTTOM);
    let top_left = hit_edges(window, (99, 49)) == Edges(Edges::LEFT | Edges::TOP);
    let top_mid = hit_edges(window, (180, 51)) == Edges(Edges::TOP);
    // The title bar's body is a move handle, not an edge.
    let title_body = hit_edges(window, (180, 61)).is_empty();
    // A point in the top-right corner would be an edge but for the buttons.
    let buttons = buttons(window);
    let over_buttons = hit_edges(window, (buttons.x + 4, buttons.y + 2)).is_empty();
    // Far outside the frame is never an edge.
    let outside = hit_edges(window, (10, 10)).is_empty();

    // Resizing keeps the opposite edge fixed when a bound clamps.
    let min = (MIN_CONTENT_W, MIN_CONTENT_H);
    let max = (300, 300);
    let area = Rect::new(0, 0, 800, 572);
    let right_grow = resize_rect(
        window,
        hit_edges(window, (299, 120)),
        (50, 0),
        (min, max),
        area,
    ) == Rect::new(100, 50, 250, 150);
    let right_max = resize_rect(
        window,
        hit_edges(window, (299, 120)),
        (1000, 0),
        (min, max),
        area,
    ) == Rect::new(100, 50, 300 + BORDER * 2, 150);
    let left_min = resize_rect(
        window,
        hit_edges(window, (101, 120)),
        (1000, 0),
        (min, max),
        area,
    ) == Rect::new(
        300 - (MIN_CONTENT_W + BORDER * 2),
        50,
        MIN_CONTENT_W + BORDER * 2,
        150,
    );
    let bottom_max = resize_rect(
        window,
        hit_edges(window, (180, 199)),
        (0, 1000),
        (min, max),
        area,
    ) == Rect::new(100, 50, 200, 300 + TITLE_H + BORDER);
    let top_min = resize_rect(
        window,
        hit_edges(window, (180, 51)),
        (0, 1000),
        (min, max),
        area,
    ) == Rect::new(
        100,
        200 - (MIN_CONTENT_H + TITLE_H + BORDER),
        200,
        MIN_CONTENT_H + TITLE_H + BORDER,
    );

    // A resize keeps the title bar reachable without moving the anchor: a
    // window already at the right reach limit cannot have its left edge
    // dragged off the screen, and a top edge stops at the work area's top.
    let far = Rect::new(800 - TITLE_REACHABLE_W, 50, 400, 150);
    let far_left = resize_rect(far, Edges(Edges::LEFT), (60, 0), (min, (1000, 1000)), area)
        == Rect::new(800 - TITLE_REACHABLE_W, 50, 400, 150);
    let top_stop = resize_rect(window, Edges(Edges::TOP), (0, -500), (min, max), area)
        == Rect::new(100, 0, 200, 200);
    // When the size bounds and reachability conflict (a window already past
    // the reach limit, e.g. after the work area shrank, at its maximum width)
    // the edge stays put rather than moving the anchor.
    let wide = Rect::new(760, 50, 390 + BORDER * 2, 150);
    let stuck = resize_rect(
        wide,
        Edges(Edges::LEFT),
        (-20, 0),
        ((380, 40), (390, 300)),
        area,
    ) == wide;

    // Off-screen movement: reachable on all four sides, body may hang off.
    let work = Rect::new(0, 0, 800, 572);
    let keep = |x, y| keep_reachable(Rect::new(x, y, 200, 150), work);
    let in_place = keep(100, 100) == (100, 100);
    let off_left = keep(-500, 100) == (-(200 - TITLE_REACHABLE_W), 100);
    let off_right = keep(900, 100) == (800 - TITLE_REACHABLE_W, 100);
    let off_top = keep(100, -100) == (100, 0);
    let off_bottom = keep(100, 1000) == (100, 572 - TITLE_H);

    // Maximized fills the work area, with and without the fallback taskbar.
    let with_bar = maximized_rect(Rect::new(0, 0, 800, 600 - 28)) == Rect::new(0, 0, 800, 572);
    let no_bar = maximized_rect(Rect::new(0, 0, 800, 600)) == Rect::new(0, 0, 800, 600);
    let clamped = clamp_into(Rect::new(-40, -40, 16, 16), Rect::new(0, 0, 800, 600))
        == Rect::new(0, 0, 16, 16);

    // Size hints clamp to the minimum and the screen, and refuse min > max.
    let hints = SizeHints::new(10, 10, 0, 0, (800, 600))
        == Some(SizeHints {
            min_w: MIN_CONTENT_W,
            min_h: MIN_CONTENT_H,
            max_w: 800 - BORDER * 2,
            max_h: 600 - TITLE_H - BORDER,
        });
    // A valid custom bound is kept, with an over-large max clamped to screen.
    let custom = SizeHints::new(200, 100, 5000, 5000, (800, 600))
        == Some(SizeHints {
            min_w: 200,
            min_h: 100,
            max_w: 800 - BORDER * 2,
            max_h: 600 - TITLE_H - BORDER,
        });
    let bad = SizeHints::new(400, 10, 100, 100, (800, 600)).is_none()
        && SizeHints::new(10, 10, 100, 100, (800, 600)).is_none();

    let ok = left
        && bottom_right
        && top_left
        && top_mid
        && title_body
        && over_buttons
        && outside
        && right_grow
        && right_max
        && left_min
        && bottom_max
        && top_min
        && far_left
        && top_stop
        && stuck
        && in_place
        && off_left
        && off_right
        && off_top
        && off_bottom
        && with_bar
        && no_bar
        && clamped
        && hints
        && custom
        && bad;
    if ok {
        "XUID:GEOM:PASS\n"
    } else {
        "XUID:GEOM:FAIL\n"
    }
}

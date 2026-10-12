#![forbid(unsafe_code)]

//! Shift-constraints for dragged shapes.

/// The end point that makes the box from `start` square (a circle for an
/// ellipse): both sides take the longer one, signs kept (a zero side extends
/// right/down).
pub fn square(start: (i32, i32), end: (i32, i32)) -> (i32, i32) {
    let dx = i64::from(end.0) - i64::from(start.0);
    let dy = i64::from(end.1) - i64::from(start.1);
    let side = dx.abs().max(dy.abs());
    let sx = if dx < 0 { -side } else { side };
    let sy = if dy < 0 { -side } else { side };
    (
        (i64::from(start.0) + sx).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        (i64::from(start.1) + sy).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    )
}

/// `end` moved to the nearest multiple of 15 degrees around `start`, keeping
/// the distance (angles measured in screen space, so 90 is straight down).
pub fn snap_angle(start: (f32, f32), end: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let length = (dx * dx + dy * dy).sqrt();
    if length == 0.0 || !length.is_finite() {
        return end;
    }
    let step = 15.0_f32.to_radians();
    let angle = (dy.atan2(dx) / step).round() * step;
    (
        start.0 + length * angle.cos(),
        start.1 + length * angle.sin(),
    )
}

/// [`snap_angle`] for pixel positions, rounded to the nearest pixel. Exact
/// multiples of 45 degrees stay exact.
pub fn snap_angle_px(start: (i32, i32), end: (i32, i32)) -> (i32, i32) {
    let (x, y) = snap_angle(
        (start.0 as f32, start.1 as f32),
        (end.0 as f32, end.1 as f32),
    );
    (x.round() as i32, y.round() as i32)
}

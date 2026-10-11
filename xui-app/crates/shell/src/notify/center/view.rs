//! The center's view for the presenter: the stack's layout, the frames to
//! draw at an instant, and whether anything is still moving.

use lazymotion::{Curve, Motion, Spring, Tween};

use super::{Center, BAR_GAP, MARGIN, REFLOW_MS, SLIDE_PX, STACK_GAP, TICK_MS};
use crate::notify::card::WIDTH;
use crate::notify::frame::{Frame, Stage, FLOURISH_DELAY_MS, FLOURISH_MS};
use crate::Rect;

impl Center {
    /// Place the measured toasts in a stack above the taskbar's top edge
    /// `bar_top`, the newest nearest it, and aim each one's tween.
    pub fn layout(&mut self, bar_top: i32, now: u64) {
        let now_ms = now * TICK_MS;
        let mut bottom = bar_top - BAR_GAP;
        let motion = self.motion;
        let spring = Curve::Spring(Spring::TOAST);
        for toast in self.live.iter_mut().rev() {
            if toast.height == 0 {
                continue;
            }
            let top = bottom - toast.height;
            bottom = top - STACK_GAP;
            if toast.at.is_none() {
                toast.at = Some(now);
                toast.arm(now);
                toast.y = if motion.travels() {
                    motion.tween(
                        top + SLIDE_PX,
                        top,
                        now_ms,
                        Spring::TOAST.settle_ms(),
                        spring,
                    )
                } else {
                    // Nothing travels: it fades in at its place (`frames`).
                    Tween::at_rest(top)
                };
            } else if toast.y.target() != top {
                toast.y = if motion.travels() {
                    motion.tween(
                        toast.y.value(now_ms),
                        top,
                        now_ms,
                        REFLOW_MS,
                        Curve::EaseOut,
                    )
                } else {
                    Tween::at_rest(top)
                };
            }
        }
    }

    /// The toasts to draw at `now`, bottom to top of the stack (oldest
    /// first), then the ones sinking out.
    pub fn frames(&self, screen_w: i32, now: u64) -> Vec<Frame> {
        let now_ms = now * TICK_MS;
        let x = screen_w - MARGIN - WIDTH;
        let mut frames = Vec::with_capacity(self.live.len() + self.leaving.len());
        for toast in &self.live {
            let Some(at) = toast.at else {
                continue;
            };
            let age_ms = ((now.saturating_sub(at)) * TICK_MS) as u32;
            let settled = toast.y.done(now_ms) && age_ms >= Spring::TOAST.settle_ms();
            frames.push(Frame {
                id: toast.id,
                rect: Rect::new(x, toast.y.value(now_ms), WIDTH, toast.height),
                stage: if settled {
                    Stage::Shown
                } else {
                    Stage::Entering
                },
                age_ms,
                fade: match self.motion {
                    Motion::Reduced => (age_ms as f32 / Motion::REDUCED_MS as f32).min(1.0),
                    _ => 1.0,
                },
                progress: toast
                    .note
                    .progress
                    .map(|_| toast.progress.value(now_ms) as f32 / 1000.0),
                countdown: (toast.hovered && toast.span > 0)
                    .then(|| toast.left.unwrap_or(0) as f32 / toast.span as f32),
                hovered: toast.hovered,
                moving: !toast.y.done(now_ms),
            });
        }
        for gone in &self.leaving {
            let fade = match gone.motion {
                Motion::Reduced => {
                    let elapsed = now.saturating_sub(gone.since) * TICK_MS;
                    1.0 - (elapsed as f32 / Motion::REDUCED_MS as f32).min(1.0)
                }
                _ => 1.0,
            };
            frames.push(Frame {
                id: gone.id,
                rect: Rect::new(x, gone.y.value(now_ms), WIDTH, gone.height),
                stage: Stage::Leaving,
                age_ms: u32::MAX / 2,
                fade,
                progress: None,
                countdown: None,
                hovered: false,
                moving: gone.motion.travels() && !gone.y.done(now_ms),
            });
        }
        frames
    }

    /// Whether anything is moving at `now`: a presenter with nothing moving
    /// and no events pending has nothing to redraw.
    pub fn animating(&self, now: u64) -> bool {
        let now_ms = now * TICK_MS;
        let flourish_end = u64::from(FLOURISH_DELAY_MS + FLOURISH_MS) / TICK_MS;
        !self.leaving.is_empty()
            || self.live.iter().any(|toast| {
                !toast.y.done(now_ms)
                    || !toast.progress.done(now_ms)
                    || toast.at.is_none()
                    || toast
                        .at
                        .is_some_and(|at| now.saturating_sub(at) < flourish_end)
            })
    }
}

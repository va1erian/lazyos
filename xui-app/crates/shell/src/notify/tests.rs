//! Host tests for the notification model: validation, identity, rate
//! limits, the queue (placement, coalescing, replace in place, timeouts),
//! the card geometry and the animation curves.

use lazymotion::Motion;
use messenger_generated::os_lazy_notify_v1 as wire;

use super::card::{self, Card, Hit};
use super::center::*;
use super::frame::{flourish, Flourish, Stage, FLOURISH_DELAY_MS, FLOURISH_MS};
use super::note::*;
use super::policy::*;
use crate::tray::policy::{Caller, Deny, Shell};

const BAR_TOP: i32 = 692;

fn args(level: u32, title: &str) -> wire::NotifyArgs {
    wire::NotifyArgs {
        level,
        title: title.into(),
        ..wire::NotifyArgs::default()
    }
}

fn req(level: Level, title: &str) -> Request {
    Request {
        note: Note::new(level, title, ""),
        replaces: None,
    }
}

/// Every character is 7 pixels wide.
fn mono(text: &str, _bold: bool) -> i32 {
    7 * text.chars().count() as i32
}

/// A centre with `n` toasts of `owner` posted and measured at `now`.
fn measured(center: &mut Center, now: u64) {
    let ids: Vec<(u64, u32)> = center
        .live()
        .iter()
        .map(|toast| (toast.id, toast.rev))
        .collect();
    for (id, rev) in ids {
        center.set_height(id, rev, 48);
    }
    center.layout(BAR_TOP, now);
}

fn closed(center: &mut Center) -> Vec<(u64, CloseReason)> {
    center
        .take_events()
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::Closed(reason) => Some((event.id, reason)),
            _ => None,
        })
        .collect()
}

mod face;
mod identity;
mod motion;
mod queue;
mod validation;

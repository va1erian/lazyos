//! Standing approvals: an administrator's approval of the elevated editor's
//! view ([`Class::View`]) covers the same caller's reads for
//! [`APPROVAL_TICKS`](crate::APPROVAL_TICKS), so it does not prompt for every
//! key it lists. Changes never stand: each one prompts.
//!
//! An approval belongs to the exact kernel-stamped caller that asked: uid,
//! label and session. Another program of the same user (a different label)
//! or the same program in another session gets none, and a logout ends the
//! session it was granted to (`elevd` follows `logind`'s session records,
//! [`crate::sessions`], and calls [`Approvals::end_session`]). `Release`
//! drops the caller's approvals at once (the Config app sends it when its
//! elevated view closes); the table is bounded and expired entries are
//! dropped first.
//!
//! **No standing approval for a label many programs share.** Every program
//! started from a shell or a script has no label (label 0), and every script
//! a developer tool runs inherits the tool's one label (`tool:<system_name>`,
//! [`devtool::is_tool_label`]): an approval for one of them would cover all
//! the others, so each of their reads prompts instead. A label `elevd` could
//! not name is treated the same way ([`LabelKind::Unknown`]): fail closed.
//! (Binding the approval to the requesting endpoint was the alternative, but
//! clients reach `elevd` through the one resolved endpoint they all share, so
//! it would not tell those programs apart either.)

use alloc::vec::Vec;

use crate::{Class, APPROVAL_TICKS};

/// Most approvals held at once.
pub const MAX_APPROVALS: usize = 16;
/// Most label kinds remembered ([`LabelKinds`]): the kernel's label table
/// holds 256, and an id is never reused within a boot.
pub const MAX_LABEL_KINDS: usize = 256;

/// The kernel-stamped identity an approval is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub label: u32,
    pub session: u64,
}

/// What a caller's kernel label says about whether an approval may stand for
/// it. `elevd` has no capability to read a label's name, so it asks `pkgd`
/// (which does) and classifies the answer with [`LabelKind::of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelKind {
    /// No label (`0`): a shell, a script, the Installer. Shared by all.
    Unlabelled,
    /// A developer tool's label: shared by every script the tool runs.
    Tool,
    /// Any other labelled caller (an installed app).
    Other,
    /// The label's name could not be read: never trusted.
    Unknown,
}

impl LabelKind {
    /// The kind of the label `label_id`, whose name is `name` when it could
    /// be read.
    pub fn of(label_id: u32, name: Option<&str>) -> LabelKind {
        match (label_id, name) {
            (0, _) => LabelKind::Unlabelled,
            (_, None) => LabelKind::Unknown,
            (_, Some(name)) if devtool::is_tool_label(name) => LabelKind::Tool,
            (_, Some(_)) => LabelKind::Other,
        }
    }

    /// Whether an approval may stand for a caller of this kind.
    pub const fn may_stand(self) -> bool {
        matches!(self, LabelKind::Other)
    }
}

/// What `elevd` has learned about label ids. `pkgd` is asked to name a label
/// (a blocking call, `elevd` being single-threaded), and a label id keeps its
/// name for the whole boot (the kernel never reuses one), so the answer is
/// asked for once: a labelled app cannot make `elevd` call `pkgd` again and
/// again, and a wedged `pkgd` stalls at most the first request of each label.
/// Only a name that was read is remembered ([`LabelKind::Unknown`] is not: it
/// may be a hiccup).
#[derive(Clone, Debug, Default)]
pub struct LabelKinds {
    known: Vec<(u32, LabelKind)>,
}

impl LabelKinds {
    pub fn new() -> LabelKinds {
        LabelKinds::default()
    }

    /// The remembered kind of `label_id`.
    pub fn get(&self, label_id: u32) -> Option<LabelKind> {
        self.known
            .iter()
            .find(|(id, _)| *id == label_id)
            .map(|(_, kind)| *kind)
    }

    /// Remember `kind` for `label_id`, unless it is [`LabelKind::Unknown`].
    /// The table is bounded; the oldest entry goes first.
    pub fn put(&mut self, label_id: u32, kind: LabelKind) {
        if kind == LabelKind::Unknown || self.get(label_id).is_some() {
            return;
        }
        if self.known.len() >= MAX_LABEL_KINDS {
            self.known.remove(0);
        }
        self.known.push((label_id, kind));
    }
}

#[derive(Clone, Copy, Debug)]
struct Approval {
    caller: Caller,
    until: u64,
}

/// The standing approvals.
#[derive(Clone, Debug, Default)]
pub struct Approvals {
    entries: Vec<Approval>,
}

impl Approvals {
    pub fn new() -> Approvals {
        Approvals::default()
    }

    /// Whether `caller` has an approval at all at `now`, of whatever class:
    /// the question to ask before the (blocking) lookup of its label's kind
    /// that [`Approvals::covers`] needs.
    pub fn holds(&self, caller: Caller, now: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.caller == caller && now < entry.until)
    }

    /// Whether `caller` (whose label is of `kind`) holds a live approval for an
    /// operation of `class`. A kind an approval may not stand for is never
    /// covered, whatever the table holds.
    pub fn covers(&self, caller: Caller, kind: LabelKind, class: Class, now: u64) -> bool {
        class == Class::View
            && kind.may_stand()
            && self
                .entries
                .iter()
                .any(|entry| entry.caller == caller && now < entry.until)
    }

    /// Record an administrator's approval for `caller` (whose label is of
    /// `kind`), when its class stands ([`Class::View`]) and an approval may
    /// stand for the label; a renewed approval restarts the clock.
    pub fn grant(&mut self, caller: Caller, kind: LabelKind, class: Class, now: u64) {
        if class != Class::View || caller.label == 0 || !kind.may_stand() {
            return;
        }
        self.entries
            .retain(|entry| entry.caller != caller && now < entry.until);
        if self.entries.len() >= MAX_APPROVALS {
            self.entries.remove(0);
        }
        self.entries.push(Approval {
            caller,
            until: now.saturating_add(APPROVAL_TICKS),
        });
    }

    /// Drop `caller`'s approvals.
    pub fn release(&mut self, caller: Caller) {
        self.entries.retain(|entry| entry.caller != caller);
    }

    /// Drop every approval of `session` (it ended).
    pub fn end_session(&mut self, session: u64) {
        self.entries.retain(|entry| entry.caller.session != session);
    }

    /// How many approvals stand at `now`.
    pub fn live(&self, now: u64) -> usize {
        self.entries
            .iter()
            .filter(|entry| now < entry.until)
            .count()
    }
}

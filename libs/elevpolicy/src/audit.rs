//! `elevd`'s audit lines: the serial `ELEVD:REQUEST` line and the record
//! `logd` journals to `/logs/elevd.log` (review of #659).
//!
//! A line must say what happened and nothing a caller forged. Every field
//! but the summary is a single token: anything outside `[A-Za-z0-9._()-]`
//! becomes `_` ([`token`]), so no field holds a space, an `=` or a line
//! break. The summary is the last field, quoted, with `\` and `"` escaped and
//! every character [`crate::text::shown`] would escape escaped too
//! ([`quoted`]): a value in it can neither end the line nor the field, so it
//! can forge neither a second `ELEVD:REQUEST` line nor an `outcome=`.

use alloc::format;
use alloc::string::String;

use crate::text::shown;

/// One audited request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line<'a> {
    /// The operation name as the caller sent it.
    pub operation: &'a str,
    pub uid: u32,
    pub label: u32,
    pub session: u64,
    /// The asker's account name (or `uid N`).
    pub user: &'a str,
    /// The administrator who approved, an account named on a refusal, or
    /// empty.
    pub admin: &'a str,
    pub outcome: &'a str,
    /// The operation's summary (`Operation::summary`).
    pub summary: &'a str,
}

/// `text` as one field token: at most 64 characters of `[A-Za-z0-9._()-]`,
/// anything else as `_`; `-` when empty.
pub fn token(text: &str) -> String {
    if text.is_empty() {
        return String::from("-");
    }
    text.chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '(' | ')' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `text` as the quoted last field: `"..."` with `\`, `"` and every
/// character the prompt could not draw escaped.
pub fn quoted(text: &str) -> String {
    format!("\"{}\"", shown(text))
}

impl Line<'_> {
    /// The serial line, without its line break:
    /// `ELEVD:REQUEST op= uid= label= session= admin= outcome= summary="..."`.
    pub fn serial(&self) -> String {
        format!(
            "ELEVD:REQUEST op={} uid={} label={} session={} admin={} outcome={} summary={}",
            token(self.operation),
            self.uid,
            self.label,
            self.session,
            token(self.admin),
            token(self.outcome),
            quoted(self.summary)
        )
    }

    /// The journal line `logd` writes to `/logs/elevd.log`:
    /// `op= outcome= uid= user= label= admin= summary="..."`.
    pub fn journal(&self) -> String {
        format!(
            "op={} outcome={} uid={} user={} label={} admin={} summary={}",
            token(self.operation),
            token(self.outcome),
            self.uid,
            token(self.user),
            self.label,
            token(self.admin),
            quoted(self.summary)
        )
    }
}

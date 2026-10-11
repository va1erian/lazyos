//! The trace line: its grammar, its channels, which of them print and how
//! often (docs/tracing-plan.md, stage T1).
//!
//! Every harness in the repository judges the OS by serial lines
//! (`INIT:AUTOSTART:PASS`, `UI:RECT x= y=`), and `dbgd` serves the same lines
//! as records. This crate is the one owner of what such a line is, shared by
//! the three runtimes that emit it (kernel, no_std services, std apps) and by
//! `dbgwire`, which parses it:
//!
//! * [`channel`]: the catalogue of who can speak (`Channel`);
//! * [`write`]: builders for the line shapes, one-line by construction;
//! * [`line`]: the parser, moved here from `dbgwire::logline`;
//! * [`mask`]: `trace.mask=` and the set of channels that print;
//! * [`budget`]: the lock-free rate limit and its `SUPPRESSED` notice;
//! * [`tracer`]: the live mask and budgets plus the [`Sink`] contract.
//!
//! Pure `no_std` + `alloc` and host-tested; the wire format is unchanged.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod budget;
pub mod channel;
pub mod fuzz;
pub mod line;
pub mod mask;
pub mod tracer;
pub mod write;

pub use budget::{Budget, Charge};
pub use channel::{Channel, Rate};
pub use line::{parse, split_complete, Line};
pub use mask::{Mask, Problem};
pub use tracer::{Sink, Tracer, LINE_MAX};
pub use write::LineBuf;

#[cfg(test)]
mod tests;

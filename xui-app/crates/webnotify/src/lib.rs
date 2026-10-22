//! A web page's notification as the shell's toast, shared by LazyWeb+ and webd (the review of
//! 2026-10-22 found webd's first version had lost LazyWeb+'s safeguards).
//!
//! * [`origin`]: whose notification it is, the toast's title (the site, never the page's words; a host
//!   too long for the title keeps its end, which a spoofed `accounts.bank.example.com.secure-...` cannot
//!   fill) and body (the page's words cleaned of controls and bidirectional overrides).
//! * [`live`]: the toasts on screen and the limits: live toasts per site and in all, a pace per site, and
//!   [`live::Bucket`], the same kind of pace for a whole client.
//!
//! Pure and host-tested: no window, no engine and no shell are needed.

pub mod live;
pub mod origin;

pub use live::{Admit, Bucket, Dropped, Live, Registry};
pub use origin::{clean, toast_body, toast_title, Origin};

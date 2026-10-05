//! The Calculator: a pocket-calculator engine ([`engine`]) and the window
//! that drives it ([`app`]), host-testable; `xui-app`'s `xui-calc` binary
//! only launches it.

pub mod app;
mod display;
pub mod engine;
pub mod keys;
pub mod number;

pub use app::{CalcApp, Msg, Report, WINDOW};
pub use engine::{Engine, Op, Press};

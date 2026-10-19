//! LazyAgent's headless core (docs/ai-agent-plan.md): everything the AI
//! agent does that is not drawing. No xui type, no LazyOS crate and no OS
//! call appears here; the front end supplies a [`Platform`]. `cargo tree -p
//! lazyagent-core` must list no `xui-*`.
//!
//! P1 holds the provider seam and the Fireworks client, the SSE parser, the
//! conversation model and its store, compaction and retries. Tools, the
//! agent loop's budgets, redaction and GitHub arrive in later phases.

pub mod compact;
pub mod core;
pub mod message;
pub mod model;
pub mod platform;
pub mod provider;
pub mod retry;
pub mod secret;
pub mod session;
pub mod settings;
pub mod sse;
pub mod store;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

pub use core::{Core, Event, Events, Failure, Request, Stats};
pub use message::{Message, Role, ToolCall, ToolSpec, Usage};
pub use platform::{Platform, Roots};
pub use provider::{
    ChatError, ChatRequest, Completion, Effort, Endpoint, Fireworks, Params, Provider, Sink,
};
pub use secret::Secret;

/// Installs `nettls-crypto` as the process's default rustls provider (the
/// GPLv2-compatible one). Call it once, before any thread starts.
pub use lazyhttps::install_tls_provider;
pub use session::{Session, TurnError};
pub use settings::{Settings, ThemeChoice};
pub use store::{Conversation, Persona, Store};

/// The app's version, for the issue footer and the `User-Agent`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

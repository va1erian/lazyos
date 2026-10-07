//! Shared code of the network apps (Network, Net Tools and Network Drives,
//! docs/networking-host-access.md).
//!
//! * [`model`]: a stack snapshot as display lines, and the configuration form
//!   checked with `netd`'s own parser (pure, host tested);
//! * [`stack`]: the typed, bounded client of `os.lazy.net.stack.v1`;
//! * [`http`]: URL parsing, the request, the response summary and the served
//!   page (pure, host tested);
//! * [`web`]: the fetch and web-server threads over `std::net`;
//! * [`drives`]: the Network Drives form and table (pure, host tested);
//! * [`mounts`]: the typed, bounded client of `os.lazy.mount.v1`.

pub mod drives;
pub mod http;
pub mod model;
pub mod mounts;
pub mod stack;
pub mod web;

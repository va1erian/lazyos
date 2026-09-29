//! Read-only Messenger fabric client for `fabricmon`: the versioned
//! `FabricStats` snapshot (syscall 5 `stats`), the kernel name registry
//! (`list`), and the userspace topics broker (`messengerd`'s
//! `os.lazy.messenger.topics` service).
//!
//! The block layouts mirror `user/src/messenger/` (which mirrors the kernel):
//! [`FabricStats`] is stats ABI v3 in the fixed little-endian word stream, the
//! registry reply is a `libmessenger` parcel whose body carries one `ENTRY`
//! record per name, and the broker reply carries one `ENTRY` per topic.
//!
//! The app is unprivileged: the snapshot is global (no handle) and the
//! registry/broker paths are ordinary syscall-5 requests, so no capability is
//! needed for a monitor.

mod broker;
mod error;
mod registry;
mod stats;

pub use broker::{Broker, TopicRow, Topics, TOPICS_NAME};
pub use error::errno_text;
pub use registry::{registry, RegistryEntry};
pub use stats::{fabric_stats, FabricStats, TaskUsage, FABRIC_STATS_SIZE, FABRIC_TASKS};

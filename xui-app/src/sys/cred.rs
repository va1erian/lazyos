//! The credential gate (syscall 10, `GET` only) and the native wall clock
//! (syscall 24, `get` only): the two kernel facts a session program such as
//! LazyShell needs besides the display and the fabric.
//!
//! Mirrors `user/src/sys/cred.rs` and `user/src/sys/wall.rs`; nothing here can
//! change an identity or the clock.

use super::native;

/// `creds(op, a1, a2)` — the audited credential gate (issue #101).
pub const SYS_CREDS: u64 = 10;
/// `wall_time(op, arg)` — the UTC wall clock (issue #369).
pub const SYS_WALL_TIME: u64 = 24;
/// Credential-gate `GET`: read a task's credential block.
const CRED_GET: u64 = 1;
/// Wall-clock `get`: UTC centiseconds since the epoch.
const WALL_GET: u64 = 0;
/// `cred_get`'s target meaning "this task".
const SELF_TARGET: u64 = u64::MAX;

/// A task's kernel-stamped identity, the userspace mirror of
/// `kernel/src/ipc/credentials.rs::Cred`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Cred {
    /// User id; `0` is root.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Capability bits (`CAP_*`).
    pub caps: u32,
    /// Policy label id.
    pub label_id: u32,
    /// Session id; `0` before login.
    pub session: u64,
}

impl Cred {
    /// The kernel's five-word block (`spawnv`'s credential words).
    pub const fn to_words(self) -> [u64; 5] {
        [
            self.uid as u64,
            self.gid as u64,
            self.caps as u64,
            self.label_id as u64,
            self.session,
        ]
    }

    /// Decode the kernel's five-word block.
    const fn from_words(words: [u64; 5]) -> Cred {
        Cred {
            uid: words[0] as u32,
            gid: words[1] as u32,
            caps: words[2] as u32,
            label_id: words[3] as u32,
            session: words[4],
        }
    }
}

/// Read `target`'s credentials (`None` = this task). The kernel lets a task
/// read its own block; reading another task's (a message sender's) needs
/// `CAP_SETUID` and is refused otherwise.
pub fn cred_get(target: Option<u64>) -> Result<Cred, i64> {
    let mut words = [0u64; 5];
    let code = native(
        SYS_CREDS,
        CRED_GET,
        target.unwrap_or(SELF_TARGET),
        words.as_mut_ptr() as u64,
    );
    if code == 0 {
        Ok(Cred::from_words(words))
    } else {
        Err(code)
    }
}

/// UTC centiseconds since the Unix epoch, from the kernel wall clock.
pub fn wall_centis() -> u64 {
    native(SYS_WALL_TIME, WALL_GET, 0, 0) as u64
}

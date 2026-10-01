//! Per-mount flags: `ro`, `noexec`, `nosuid`.

use alloc::string::String;

/// What a mount forbids. The default allows everything.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MountFlags {
    /// Every mutating entry point answers [`super::FsError::ReadOnly`] before
    /// the backend is called.
    pub ro: bool,
    /// Binaries on this mount are not executed (native spawn, Linux `execve`).
    pub noexec: bool,
    /// Recorded and reported only: the kernel honours no setuid-on-exec yet.
    pub nosuid: bool,
}

impl MountFlags {
    /// Parse one flag name; `None` for an unknown one.
    pub fn parse_one(name: &str) -> Option<MountFlags> {
        let mut flags = MountFlags::default();
        match name {
            "ro" => flags.ro = true,
            "noexec" => flags.noexec = true,
            "nosuid" => flags.nosuid = true,
            _ => return None,
        }
        Some(flags)
    }

    /// Parse a comma list such as `ro,noexec`. An empty list is no flags; an
    /// unknown or empty element is an error carrying the offending text.
    pub fn parse_list(list: &str) -> Result<MountFlags, &str> {
        let mut flags = MountFlags::default();
        if list.is_empty() {
            return Ok(flags);
        }
        for name in list.split(',') {
            flags = flags.union(MountFlags::parse_one(name).ok_or(name)?);
        }
        Ok(flags)
    }

    /// Both sets of restrictions.
    pub fn union(self, other: MountFlags) -> MountFlags {
        MountFlags {
            ro: self.ro || other.ro,
            noexec: self.noexec || other.noexec,
            nosuid: self.nosuid || other.nosuid,
        }
    }

    /// The `/proc/mounts` option words these flags add, each preceded by a comma.
    pub fn proc_suffix(self) -> String {
        let mut text = String::new();
        for (set, word) in [(self.noexec, ",noexec"), (self.nosuid, ",nosuid")] {
            if set {
                text.push_str(word);
            }
        }
        text
    }
}

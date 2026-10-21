//! What `init` remembers about a developer tool's stored manifest
//! (`user/src/bin/init/installed.rs`, `toolgate::stored_loadable`).
//!
//! `init` re-reads the installed apps on every `Launch` and `ListApps`, and a
//! tool row costs a manifest read and a parse. The verdict is a fact about one
//! version of the row, so it is kept by `(system_name, version key)` where the
//! key names the digest and the row fields the verdict reads: a reinstall or a
//! changed row is judged again, anything else is answered from here. An
//! agreement and a refusal are both remembered; an unreadable manifest is
//! neither (it may be a hiccup). Each package has at most one entry of each
//! kind, and [`Verdicts::forget_removed`] drops the packages that are no
//! longer installed, so the table is bounded by the installed apps and a
//! removed package leaves nothing behind.

use alloc::string::String;
use alloc::vec::Vec;

/// A remembered verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The stored manifest agreed with the row: the tool may be started.
    Agreed,
    /// `stored_loadable` refused it.
    Refused,
}

/// The remembered verdicts, one entry per package.
#[derive(Default)]
pub struct Verdicts {
    entries: Vec<(String, String, Verdict)>,
}

impl Verdicts {
    pub const fn new() -> Verdicts {
        Verdicts {
            entries: Vec::new(),
        }
    }

    /// The verdict remembered for this version of the package, if any.
    pub fn get(&self, system_name: &str, key: &str) -> Option<Verdict> {
        self.entries
            .iter()
            .find(|(name, version, _)| name == system_name && version == key)
            .map(|(_, _, verdict)| *verdict)
    }

    /// Remember `verdict` for this version, replacing the package's older one.
    pub fn remember(&mut self, system_name: &str, key: &str, verdict: Verdict) {
        self.entries.retain(|(name, _, _)| name != system_name);
        self.entries
            .push((String::from(system_name), String::from(key), verdict));
    }

    /// Drop the entries of every package not in `names`.
    pub fn forget_removed(&mut self, names: &[String]) {
        self.entries.retain(|(name, _, _)| names.contains(name));
    }

    /// How many packages are remembered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What a verdict on `row` depends on, as one string: the digest and the row
/// fields `toolgate::stored_loadable` reads (`kind`, `machine_wide`, `core`).
pub fn version_key(digest: &str, kind: &str, machine_wide: bool, core: bool) -> String {
    alloc::format!("{digest}/{kind}/{machine_wide}/{core}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| String::from(*name)).collect()
    }

    #[test]
    fn a_verdict_is_kept_for_its_version_only() {
        let mut verdicts = Verdicts::new();
        assert_eq!(verdicts.get("os.lazy.tool", "a"), None);
        verdicts.remember("os.lazy.tool", "a", Verdict::Refused);
        assert_eq!(verdicts.get("os.lazy.tool", "a"), Some(Verdict::Refused));
        // A reinstall (another digest) is judged again.
        assert_eq!(verdicts.get("os.lazy.tool", "b"), None);
        assert_eq!(verdicts.get("os.lazy.other", "a"), None);
    }

    #[test]
    fn a_new_verdict_replaces_the_packages_old_one() {
        let mut verdicts = Verdicts::new();
        verdicts.remember("os.lazy.tool", "a", Verdict::Refused);
        verdicts.remember("os.lazy.tool", "b", Verdict::Agreed);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts.get("os.lazy.tool", "a"), None);
        assert_eq!(verdicts.get("os.lazy.tool", "b"), Some(Verdict::Agreed));
    }

    #[test]
    fn a_removed_package_leaves_nothing_behind() {
        let mut verdicts = Verdicts::new();
        verdicts.remember("os.lazy.one", "a", Verdict::Agreed);
        verdicts.remember("os.lazy.two", "a", Verdict::Refused);
        verdicts.forget_removed(&names(&["os.lazy.one"]));
        assert_eq!(verdicts.get("os.lazy.one", "a"), Some(Verdict::Agreed));
        assert_eq!(verdicts.get("os.lazy.two", "a"), None);
        verdicts.forget_removed(&[]);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn install_remove_cycles_stay_bounded() {
        // A package installed and removed a thousand times (each time under a
        // new digest, as a reinstall has) never grows the table.
        let mut verdicts = Verdicts::new();
        for round in 0..1000u32 {
            let key = version_key(
                &alloc::format!("{round:064x}"),
                "developer-tool",
                true,
                false,
            );
            verdicts.remember("os.lazy.churn", &key, Verdict::Refused);
            verdicts.remember("os.lazy.steady", "k", Verdict::Agreed);
            assert!(verdicts.len() <= 2);
            if round % 2 == 1 {
                verdicts.forget_removed(&names(&["os.lazy.steady"]));
                assert_eq!(verdicts.len(), 1);
            }
        }
    }

    #[test]
    fn the_key_names_every_field_the_verdict_reads() {
        let base = version_key("d", "developer-tool", true, false);
        for other in [
            version_key("e", "developer-tool", true, false),
            version_key("d", "", true, false),
            version_key("d", "developer-tool", false, false),
            version_key("d", "developer-tool", true, true),
        ] {
            assert_ne!(base, other);
        }
    }
}

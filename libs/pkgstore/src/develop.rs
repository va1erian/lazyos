//! Development labels (issue #529, `docs/lazyrad-package-plan.md` section 3):
//! what `pkgd`'s `Develop` loads for an app being developed, and the
//! in-memory record of what the user already approved.
//!
//! An IDE runs the project it edits as a child labelled `dev:<system_name>`,
//! so the project is judged by the permissions its manifest declares instead
//! of the IDE's. The kernel lets a labelled IDE enter such a label only while
//! the label holds rules, so the rule set always ends with [`SENTINEL`], a
//! catch-all deny: it changes no decision (an unmatched call is denied anyway,
//! and it is last, so it shadows nothing), but an approved app that asks for
//! nothing still holds one rule, and revoking (loading none) closes the label.
//!
//! [`Approvals`] remembers, per label, the widest rule set a session approved:
//! the same or a narrower set is approved again without asking, a wider one
//! needs the consent screen. Nothing here is persisted; a session's approvals
//! go when it logs out ([`Approvals::end_session`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;

use crate::rules::{self, CompileError, ANY_METHOD, MAX_RULES};

/// The wildcard interface of a rule (`0xFFFFFFFFFFFFFFFF`).
pub const ANY_INTERFACE: u64 = u64::MAX;

/// The catch-all deny every approved development rule set ends with.
pub const SENTINEL: LabelRule = LabelRule {
    interface_id: ANY_INTERFACE,
    method: ANY_METHOD,
    allow: false,
};

/// Most labels remembered at once; approving one more forgets (and the caller
/// revokes) the oldest.
pub const MAX_APPROVALS: usize = 32;

/// The prefix of every development label.
pub const DEV_PREFIX: &str = "dev:";

/// The development label of `system_name`.
pub fn label(system_name: &str) -> String {
    format!("{DEV_PREFIX}{system_name}")
}

/// The rules `Develop` loads for `manifest`'s development label: the same
/// compilation an install uses, then [`SENTINEL`].
pub fn rules(manifest: &Manifest) -> Result<Vec<LabelRule>, CompileError> {
    let mut compiled = rules::compile(manifest)?;
    if compiled.len() >= MAX_RULES {
        return Err(CompileError::TooManyRules {
            rules: compiled.len() + 1,
        });
    }
    compiled.push(SENTINEL);
    Ok(compiled)
}

/// One approved label.
#[derive(Clone, Debug, PartialEq)]
struct Approval {
    label: String,
    /// The login session that approved it.
    session: u64,
    /// The widest rule set approved for the label in that session.
    rules: Vec<LabelRule>,
}

/// What `Develop` should do with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The session already approved these rules or wider: load them.
    Approved,
    /// The user must see the consent screen first.
    NeedsConsent,
}

/// The approvals `pkgd` holds in memory.
#[derive(Debug, Default)]
pub struct Approvals {
    entries: Vec<Approval>,
}

impl Approvals {
    pub const fn new() -> Approvals {
        Approvals {
            entries: Vec::new(),
        }
    }

    /// Whether nothing is approved (so nothing needs revoking at logout).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The labels currently approved, oldest first.
    pub fn labels(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.label.clone())
            .collect()
    }

    /// Whether `session` may load `rules` for `label` without asking: it
    /// approved the label before, and every rule now asked for was in the set
    /// it approved.
    pub fn check(&self, label: &str, session: u64, rules: &[LabelRule]) -> Verdict {
        let covered = self
            .entries
            .iter()
            .find(|entry| entry.label == label && entry.session == session)
            .is_some_and(|entry| rules.iter().all(|rule| entry.rules.contains(rule)));
        if covered {
            Verdict::Approved
        } else {
            Verdict::NeedsConsent
        }
    }

    /// Record the user's yes for `rules` on `label` in `session`. An earlier
    /// approval of the same label in the same session is widened (its rules
    /// stay approved), one from another session is replaced. Returns the
    /// label forgotten to make room, which the caller revokes.
    pub fn approve(&mut self, label: &str, session: u64, rules: &[LabelRule]) -> Option<String> {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.label == label) {
            if entry.session != session {
                entry.session = session;
                entry.rules.clear();
            }
            for rule in rules {
                if !entry.rules.contains(rule) {
                    entry.rules.push(rule.clone());
                }
            }
            return None;
        }
        let evicted = (self.entries.len() >= MAX_APPROVALS).then(|| self.entries.remove(0).label);
        self.entries.push(Approval {
            label: String::from(label),
            session,
            rules: rules.to_vec(),
        });
        evicted
    }

    /// Forget every approval `session` gave; returns their labels, which the
    /// caller revokes in the kernel.
    pub fn end_session(&mut self, session: u64) -> Vec<String> {
        let mut ended = Vec::new();
        self.entries.retain(|entry| {
            let keep = entry.session != session;
            if !keep {
                ended.push(entry.label.clone());
            }
            keep
        });
        ended
    }

    /// Forget every approval whose session is not in `live`, the sessions
    /// `logind` reports active; `None` (the list is unavailable) forgets them
    /// all, so a lost logout can never leave a label approved. Returns each
    /// forgotten label with its session, which the caller revokes.
    pub fn keep_live(&mut self, live: Option<&[u64]>) -> Vec<(u64, String)> {
        let mut ended = Vec::new();
        self.entries.retain(|entry| {
            let keep = live.is_some_and(|live| live.contains(&entry.session));
            if !keep {
                ended.push((entry.session, entry.label.clone()));
            }
            keep
        });
        ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::fnv1a64;
    use messenger_generated::os_lazy_process_label_spawn_v1 as spawn_scope;

    fn manifest(permissions: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n[permissions]\n{permissions}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    fn allow(name: &str) -> LabelRule {
        LabelRule {
            interface_id: fnv1a64(name),
            method: ANY_METHOD,
            allow: true,
        }
    }

    #[test]
    fn the_label_is_dev_colon_system_name() {
        assert_eq!(label("org.lazy.demo"), "dev:org.lazy.demo");
    }

    #[test]
    fn an_empty_manifest_still_holds_the_sentinel() {
        assert_eq!(rules(&manifest("")).unwrap(), [SENTINEL]);
    }

    #[test]
    fn the_sentinel_comes_last_after_the_install_rules() {
        let manifest = manifest("interfaces = [\"os.lazy.display.v1\"]\n");
        let rules = rules(&manifest).unwrap();
        let installed = rules::compile(&manifest).unwrap();
        assert_eq!(&rules[..installed.len()], &installed[..]);
        assert_eq!(rules.last(), Some(&SENTINEL));
        assert!(rules[..rules.len() - 1].iter().all(|rule| rule.allow));
    }

    #[test]
    fn the_largest_rule_set_still_has_room_for_the_sentinel() {
        // 84 interfaces compile to 252 rules, the most that leaves room for
        // the 3 baseline rules an install adds; the sentinel fits in that room.
        let fits: Vec<String> = (0..84).map(|i| format!("\"x.y{i}.v1\"")).collect();
        let text = format!("interfaces = [{}]\n", fits.join(", "));
        assert_eq!(rules(&manifest(&text)).unwrap().len(), 253);
        // One more interface is over.
        let over: Vec<String> = (0..85).map(|i| format!("\"x.y{i}.v1\"")).collect();
        let text = format!("interfaces = [{}]\n", over.join(", "));
        assert!(matches!(
            rules(&manifest(&text)),
            Err(CompileError::TooManyRules { .. })
        ));
    }

    #[test]
    fn develop_compiles_to_the_spawn_scope() {
        let rules = rules::compile(&manifest("develop = true\n")).unwrap();
        assert_eq!(
            rules,
            [LabelRule {
                interface_id: spawn_scope::INTERFACE_ID,
                method: ANY_METHOD,
                allow: true,
            }]
        );
        assert!(rules::compile(&manifest("develop = false\n"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unchanged_or_narrower_is_approved_wider_needs_consent() {
        let mut approvals = Approvals::new();
        let a = || allow("os.lazy.a.v1");
        let b = || allow("os.lazy.b.v1");
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[a()]),
            Verdict::NeedsConsent
        );
        approvals.approve("dev:x.y.z", 1, &[a(), b(), SENTINEL]);
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[a(), b(), SENTINEL]),
            Verdict::Approved
        );
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[a(), SENTINEL]),
            Verdict::Approved
        );
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[SENTINEL]),
            Verdict::Approved
        );
        let c = || allow("os.lazy.c.v1");
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[a(), c(), SENTINEL]),
            Verdict::NeedsConsent
        );
        // A narrower run does not shrink what was approved.
        approvals.approve("dev:x.y.z", 1, &[a(), SENTINEL]);
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[a(), b(), SENTINEL]),
            Verdict::Approved
        );
        // Another label or another session is not covered.
        assert_eq!(
            approvals.check("dev:x.y.w", 1, &[SENTINEL]),
            Verdict::NeedsConsent
        );
        assert_eq!(
            approvals.check("dev:x.y.z", 2, &[SENTINEL]),
            Verdict::NeedsConsent
        );
    }

    #[test]
    fn another_session_replaces_the_approval() {
        let mut approvals = Approvals::new();
        let a = || allow("os.lazy.a.v1");
        approvals.approve("dev:x.y.z", 1, &[a(), SENTINEL]);
        approvals.approve("dev:x.y.z", 2, &[SENTINEL]);
        assert_eq!(
            approvals.check("dev:x.y.z", 2, &[a()]),
            Verdict::NeedsConsent
        );
        assert_eq!(
            approvals.check("dev:x.y.z", 1, &[SENTINEL]),
            Verdict::NeedsConsent
        );
        assert_eq!(approvals.labels(), ["dev:x.y.z"]);
    }

    #[test]
    fn logout_drops_only_that_sessions_labels() {
        let mut approvals = Approvals::new();
        approvals.approve("dev:a.b.c", 1, &[SENTINEL]);
        approvals.approve("dev:a.b.d", 2, &[SENTINEL]);
        approvals.approve("dev:a.b.e", 1, &[SENTINEL]);
        assert_eq!(approvals.end_session(1), ["dev:a.b.c", "dev:a.b.e"]);
        assert_eq!(approvals.labels(), ["dev:a.b.d"]);
        assert!(approvals.end_session(1).is_empty());
        assert_eq!(approvals.end_session(2), ["dev:a.b.d"]);
        assert!(approvals.is_empty());
    }

    #[test]
    fn reconciling_keeps_only_live_sessions() {
        let mut approvals = Approvals::new();
        approvals.approve("dev:a.b.c", 1, &[SENTINEL]);
        approvals.approve("dev:a.b.d", 2, &[SENTINEL]);
        approvals.approve("dev:a.b.e", 3, &[SENTINEL]);
        let ended = approvals.keep_live(Some(&[2, 9]));
        assert_eq!(
            ended,
            [
                (1, String::from("dev:a.b.c")),
                (3, String::from("dev:a.b.e"))
            ]
        );
        assert_eq!(approvals.labels(), ["dev:a.b.d"]);
        assert!(approvals.keep_live(Some(&[2])).is_empty());
    }

    #[test]
    fn reconciling_without_the_session_list_forgets_everything() {
        let mut approvals = Approvals::new();
        approvals.approve("dev:a.b.c", 1, &[SENTINEL]);
        approvals.approve("dev:a.b.d", 2, &[SENTINEL]);
        assert_eq!(approvals.keep_live(None).len(), 2);
        assert!(approvals.is_empty());
    }

    #[test]
    fn a_full_table_forgets_the_oldest_and_says_which() {
        let mut approvals = Approvals::new();
        for index in 0..MAX_APPROVALS {
            assert_eq!(
                approvals.approve(&format!("dev:a.b.n{index}"), 1, &[SENTINEL]),
                None
            );
        }
        assert_eq!(
            approvals.approve("dev:a.b.new", 1, &[SENTINEL]).as_deref(),
            Some("dev:a.b.n0")
        );
        assert_eq!(approvals.labels().len(), MAX_APPROVALS);
        assert_eq!(
            approvals.check("dev:a.b.n0", 1, &[SENTINEL]),
            Verdict::NeedsConsent
        );
    }
}

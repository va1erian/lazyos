//! `Develop`: approve a development run of a package (issue #529,
//! `docs/lazyrad-package-plan.md` section 3).
//!
//! An IDE that is itself a package runs the project it edits as a child
//! labelled `dev:<system_name>`, so the project is judged by its own manifest
//! instead of the IDE's. The kernel lets the IDE enter that label only while it
//! holds rules (`kernel/src/ipc/devspawn.rs`), and only `pkgd` loads them: here,
//! after the user approved them on the Installer's consent screen.
//!
//! Who may ask is `Install`'s rule (`pkgstore::access::may_manage`): the
//! unlabelled Installer or another session-owner tool, never the IDE itself.
//! What was approved is kept per label and session in memory
//! (`pkgstore::develop::Approvals`): the same or a narrower rule set is loaded
//! again without asking, anything wider needs the consent screen. Nothing is
//! persisted, and when the session logs out (`system/events/login/end` on
//! `init`'s broker) its labels are revoked: an empty rule list, which the
//! kernel reads as "not approved". A forged logout event can only cause a
//! second prompt.
//!
//! Evidence: `PKGD:DEVELOP:PASS <label> rules=<n> asked=<0|1>`,
//! `PKGD:DEVELOP:ASK <label>` (consent needed), `PKGD:DEVELOP:FAIL <why>`,
//! `PKGD:UNDEVELOP:PASS <label> session=<id>`; every approval and revocation is
//! a `develop` / `undevelop` record in `pkg.log`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use pkgstore::access::Caller;
use pkgstore::develop::{self, Verdict};
use user::messenger::pkgd::Failure;
use user::messenger::{self, logind, policy, router, services};
use user::sys;

use super::handlers::{fail, read_failure, Pkgd, EINVAL, EIO, EPERM};
use super::inspect::assess;
use super::install::{event, one_line, problem_text, Subject};
use super::store;

/// Ticks between two looks at the logout feed while approvals are held.
pub(crate) const LOGOUT_POLL_TICKS: u64 = 50;

/// The logout feed: `system/events/login/end` on `init`'s broker.
pub(crate) struct Logouts {
    bus: Option<router::Bus>,
    feed: Option<router::Subscriber>,
    buffer: Vec<u8>,
}

impl Logouts {
    pub(crate) const fn new() -> Logouts {
        Logouts {
            bus: None,
            feed: None,
            buffer: Vec::new(),
        }
    }

    /// The sessions that ended since the last call (subscribing first if
    /// needed; a broker that is not up yet is retried next time).
    fn ended(&mut self) -> Vec<u64> {
        if self.feed.is_none() {
            if self.bus.is_none() {
                self.bus = router::Bus::connect(services::INIT_NAME).ok();
            }
            let topic = logind::wire::TOPIC_SYSTEM_EVENTS_LOGIN_END;
            self.feed = self.bus.as_ref().and_then(|bus| bus.subscribe(topic).ok());
        }
        if self.buffer.is_empty() {
            self.buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
        }
        let mut ended = Vec::new();
        let Some(feed) = &self.feed else {
            return ended;
        };
        loop {
            match feed.recv_with(&mut self.buffer, Some(messenger::EXPIRED_DEADLINE)) {
                Ok(Some(event)) => {
                    if let Ok(end) = logind::wire::decode_login_end(&event.payload) {
                        ended.push(end.session);
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    // The broker restarted: subscribe again next time.
                    self.feed = None;
                    self.bus = None;
                    break;
                }
            }
        }
        ended
    }
}

impl Pkgd {
    /// `Develop(path, confirm)`; see `idl/pkgd.midl`.
    pub(crate) fn develop(
        &mut self,
        caller: &Caller,
        path: &str,
        confirm: bool,
    ) -> Result<(String, bool), Failure> {
        let uid = u64::from(caller.uid);
        let nobody = Subject::none();
        if let Err(why) = pkgstore::access::may_manage(caller) {
            return Err(self.refuse(&nobody, uid, "DEVELOP", fail(EPERM, why)));
        }
        let path = match self.check_source(caller, path) {
            Ok(path) => path,
            Err(failure) => return Err(self.refuse(&nobody, uid, "DEVELOP", failure)),
        };
        if let Err(code) = store::read_package(&mut self.buffer, &path) {
            return Err(self.refuse(&nobody, uid, "DEVELOP", read_failure(code)));
        }
        let bytes = core::mem::take(&mut self.buffer);
        let outcome = self.develop_bytes(&bytes, caller, confirm);
        self.buffer = bytes;
        outcome
    }

    fn develop_bytes(
        &mut self,
        bytes: &[u8],
        caller: &Caller,
        confirm: bool,
    ) -> Result<(String, bool), Failure> {
        let uid = u64::from(caller.uid);
        let assessed = match assess(bytes) {
            Ok(assessed) if assessed.info.problems.is_empty() => assessed,
            Ok(assessed) => {
                let failure = fail(EINVAL, problem_text(&assessed.info.problems));
                return Err(self.refuse(&Subject::none(), uid, "DEVELOP", failure));
            }
            Err(info) => {
                let failure = fail(EINVAL, problem_text(&info.problems));
                return Err(self.refuse(&Subject::none(), uid, "DEVELOP", failure));
            }
        };
        let info = &assessed.info;
        let subject = Subject {
            system_name: info.system_name.clone(),
            version: info.version.clone(),
            install_dir: String::new(),
            digest: info.digest.clone(),
        };
        // A development run owns the app's names and topics: never a built-in
        // app's, whatever the package claims.
        if self.is_core(&info.system_name) {
            let failure = fail(
                EPERM,
                "a built-in application cannot be run as a development build",
            );
            return Err(self.refuse(&subject, uid, "DEVELOP", failure));
        }
        let label = develop::label(&info.system_name);
        let rules = match develop::rules(assessed.package.manifest()) {
            Ok(rules) => rules,
            Err(error) => {
                let failure = fail(EINVAL, format!("{error}"));
                return Err(self.refuse(&subject, uid, "DEVELOP", failure));
            }
        };
        let verdict = self.approvals.check(&label, caller.session, &rules);
        if verdict == Verdict::NeedsConsent && !confirm {
            sys::write_str(&format!("PKGD:DEVELOP:ASK {label}\n"));
            return Ok((label, false));
        }
        if let Err(error) = policy::load_label(&label, &rules) {
            let failure = fail(
                EIO,
                format!(
                    "the system would not accept the app's permissions: {}",
                    error.message()
                ),
            );
            return Err(self.refuse(&subject, uid, "DEVELOP", failure));
        }
        let asked = verdict == Verdict::NeedsConsent;
        if asked {
            if let Some(evicted) = self.approvals.approve(&label, caller.session, &rules) {
                self.revoke_dev(&evicted, caller.session, uid);
            }
        }
        self.audit
            .record(&event("develop", &subject, uid, true, &label));
        sys::write_str(&format!(
            "PKGD:DEVELOP:PASS {label} rules={} asked={}\n",
            rules.len(),
            u8::from(asked)
        ));
        Ok((label, true))
    }

    /// Revoke `label` (an empty rule list) and audit it.
    fn revoke_dev(&mut self, label: &str, session: u64, uid: u64) {
        let ok = policy::load_label(label, &[]).is_ok();
        let mut subject = Subject::none();
        subject.system_name = String::from(label.trim_start_matches("dev:"));
        self.audit
            .record(&event("undevelop", &subject, uid, ok, label));
        let verdict = if ok { "PASS" } else { "FAIL" };
        sys::write_str(&format!(
            "PKGD:UNDEVELOP:{verdict} {} session={session}\n",
            one_line(label)
        ));
    }

    /// Whether any development label is approved (the service loop then
    /// wakes up to watch for logouts).
    pub(crate) fn holds_dev_approvals(&self) -> bool {
        !self.approvals.is_empty()
    }

    /// Revoke the approvals of every session that logged out.
    pub(crate) fn watch_logouts(&mut self) {
        if self.approvals.is_empty() {
            return;
        }
        for session in self.logouts.ended() {
            for label in self.approvals.end_session(session) {
                self.revoke_dev(&label, session, 0);
            }
        }
    }
}

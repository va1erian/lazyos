//! Core package provisioning (issue #509 §3): after reconciliation, `pkgd`
//! brings `/apps` in line with what the image ships in `/system/packages`.
//!
//! The decisions are `pkgstore::provision`'s (host-tested); this module reads
//! the shipped set, runs the steps one at a time, and records the result.
//! [`Pkgd::begin_provisioning`] runs before the request loop; the loop then
//! calls [`Pkgd::provision_step`] between requests, so `Provisioned`, `List`
//! and `Inspect` are answered while it works (`init` polls `Provisioned`
//! before it autostarts apps) and `Install`/`Remove` are refused with
//! `EAGAIN` until it is done.
//!
//! Serial evidence: `PKGD:PROVISION:INSTALL|UPGRADE <system_name>`,
//! `PKGD:PROVISION:KEEP sn=<..> installed=<v> shipped=<v>`,
//! `PKGD:PROVISION:FAIL sn=<..> reason=<..>`, and once per start
//! `PKGD:PROVISION:DONE installed=<n> upgraded=<n> kept=<n> failed=<n> free=<bytes>`.
//! Every step is audited in `pkg.log` as `op=provision`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hex;
use pkgstore::provision::{self, Action, Current, Shipped, Tally};
use user::files;
use user::messenger::pkgd::{wire, Failure, PkgEvent};
use user::messenger::{self, router, services};
use user::sys;

use super::handlers::{fail, read_failure, Pkgd, EINVAL, ENOENT};
use super::inspect::assess;
use super::install::{event, one_line, problem_text, Subject};
use super::store;

/// Tell `init` how far provisioning is (`ready`, then `done`): its autostart
/// waits for it (`user/src/bin/init/provisioning.rs`). The event goes through
/// `init`'s own topic broker, which `init` reads as it serves it, so it never
/// has to call `pkgd` (which answers only between two packages). Best effort:
/// on failure `init` opens the apps after its bounded wait.
fn announce(event: &PkgEvent) {
    for _ in 0..ANNOUNCE_ATTEMPTS {
        if let Ok(mut bus) = router::Bus::connect(services::INIT_NAME) {
            let sent = wire::publish_system_events_pkg(&mut bus, &event.op, event).is_ok();
            let _ = bus.endpoint().release();
            if sent {
                return;
            }
        }
        messenger::park_tick();
    }
}

/// Attempts at reaching `init`'s broker.
const ANNOUNCE_ATTEMPTS: usize = 8;

/// The steps of this start's pass that are still to run.
pub(crate) struct Pass {
    actions: Vec<Action>,
    next: usize,
    tally: Tally,
    stamp: String,
    /// Steps left before every package that opens at login is provisioned.
    autostart_left: usize,
}

impl Pkgd {
    /// Learn the shipped set, compare it with the rows and plan the pass.
    /// A boot whose set did not change finishes here.
    pub(crate) fn begin_provisioning(&mut self, volume_ok: bool) {
        let (shipped, sizes, unreadable) = self.shipped_set();
        self.core = shipped;
        // An archive that cannot even be described is a failed package.
        let start = Tally {
            failed: unreadable,
            ..Tally::default()
        };
        if !volume_ok {
            sys::write_str("PKGD:PROVISION:SKIP the application store is unavailable\n");
            self.finish(None, start);
            return;
        }
        let rows = match self.registry.list() {
            Ok(rows) => rows,
            Err(error) => {
                sys::write_str(&format!(
                    "PKGD:PROVISION:SKIP the application list is unavailable: {}\n",
                    error.message()
                ));
                self.finish(None, start);
                return;
            }
        };
        let current: Vec<Current> = rows
            .iter()
            .map(|row| Current {
                system_name: row.system_name.clone(),
                version: row.version.clone(),
                digest: row.digest.clone(),
                core: row.origin == wire::ORIGIN_CORE,
            })
            .collect();
        let stored = self.registry.stamp().ok().flatten();
        if provision::up_to_date(stored.as_deref(), &self.core, &current) {
            self.finish(None, start);
            return;
        }
        let mut actions = provision::plan(&self.core, &current);
        let size = |name: &str| {
            sizes
                .iter()
                .find(|(n, _)| n == name)
                .map_or(0, |(_, size)| *size)
        };
        provision::largest_first(&mut actions, size);
        let core = &self.core;
        let autostart = |name: &str| core.iter().any(|p| p.system_name == name && p.autostart);
        provision::autostart_first(&mut actions, autostart);
        let autostart_left = provision::autostart_steps(&actions, autostart);
        // Sized once for the largest package, so every read reuses it.
        let largest = sizes.iter().map(|(_, size)| *size).max().unwrap_or(0);
        if self.buffer.capacity() < largest {
            self.buffer.clear();
            self.buffer.reserve_exact(largest);
        }
        self.pass = Some(Pass {
            actions,
            next: 0,
            tally: start,
            stamp: provision::stamp(&self.core),
            autostart_left,
        });
        self.provisioned.ready = autostart_left == 0;
        if self.provisioned.ready {
            // Nothing that opens at login needs installing: say so at once.
            announce(&event("provision", &Subject::none(), 0, true, "ready"));
        }
    }

    /// Run one step of the pass; the last one records the stamp.
    pub(crate) fn provision_step(&mut self) {
        let Some(mut pass) = self.pass.take() else {
            return;
        };
        if let Some(action) = pass.actions.get(pass.next).cloned() {
            pass.next += 1;
            self.run(action, &mut pass.tally);
            // `Provisioned` reports the progress while the pass runs (the
            // compositor prints it on the console before the desktop).
            self.provisioned.installed = pass.tally.installed;
            self.provisioned.upgraded = pass.tally.upgraded;
            self.provisioned.kept = pass.tally.kept;
            self.provisioned.failed = pass.tally.failed;
            pass.autostart_left = pass.autostart_left.saturating_sub(1);
            if pass.autostart_left == 0 && !self.provisioned.ready {
                self.provisioned.ready = true;
                sys::write_str("PKGD:PROVISION:READY the apps that open at login are installed\n");
                let ready = event("provision", &Subject::none(), 0, true, "ready");
                announce(&ready);
            }
            self.pass = Some(pass);
            return;
        }
        if let Err(error) = self.registry.set_stamp(&pass.stamp) {
            sys::write_str(&format!(
                "PKGD:PROVISION:STAMP:FAIL {}\n",
                one_line(error.message())
            ));
        }
        let summary = format!(
            "installed={} upgraded={} kept={} failed={}",
            pass.tally.installed, pass.tally.upgraded, pass.tally.kept, pass.tally.failed
        );
        self.finish(Some(summary), pass.tally);
    }

    /// The pass is over: report it, publish it and open for business.
    fn finish(&mut self, summary: Option<String>, tally: Tally) {
        self.provisioned = wire::ProvisionState {
            done: true,
            ready: true,
            installed: tally.installed,
            upgraded: tally.upgraded,
            kept: tally.kept,
            failed: tally.failed,
        };
        let free = files::fs_space(pkgstore::layout::APPS_ROOT)
            .ok()
            .map(|(_, free)| free);
        sys::write_str(&tally.done_line(free));
        let mut done = event("provision", &Subject::none(), 0, tally.failed == 0, "done");
        match summary {
            // A pass that did something ends with a record of it.
            Some(summary) => {
                done.detail = format!("done {summary}");
                self.audit.record(&done);
            }
            // A no-op boot announces itself without growing the log.
            None => self.audit.publish(&done),
        }
        announce(&done);
    }

    fn run(&mut self, action: Action, tally: &mut Tally) {
        match action {
            Action::Install(name) | Action::Upgrade(name) => {
                let started = sys::clock();
                self.provision_counted(&name, started, tally)
            }
            Action::Keep {
                system_name,
                installed,
                shipped,
            } => {
                sys::write_str(&format!(
                    "PKGD:PROVISION:KEEP sn={system_name} installed={installed} shipped={shipped}\n"
                ));
                // A newer version over a core app is still core.
                self.set_origin(&system_name, wire::ORIGIN_CORE, "kept a newer version");
                tally.kept += 1;
            }
            Action::MarkCore(name) => {
                self.set_origin(&name, wire::ORIGIN_CORE, "recorded as built-in");
            }
            Action::Demote(name) => {
                sys::write_str(&format!("PKGD:PROVISION:DEMOTE {name}\n"));
                self.set_origin(&name, wire::ORIGIN_USER, "no longer built in");
            }
        }
    }

    /// Install or upgrade one shipped package and count it; the marker says
    /// how many ticks (10 ms) it took.
    fn provision_counted(&mut self, name: &str, started: u64, tally: &mut Tally) {
        match self.provision_one(name) {
            Ok(upgrade) => {
                let verb = if upgrade { "UPGRADE" } else { "INSTALL" };
                let ticks = sys::clock().saturating_sub(started);
                sys::write_str(&format!("PKGD:PROVISION:{verb} {name} ticks={ticks}\n"));
                if upgrade {
                    tally.upgraded += 1;
                } else {
                    tally.installed += 1;
                }
            }
            Err(failure) => {
                sys::write_str(&format!(
                    "PKGD:PROVISION:FAIL sn={name} reason=\"{}\"\n",
                    one_line(&failure.text)
                ));
                tally.failed += 1;
            }
        }
    }

    /// Rewrite one row's origin (no files change); audited.
    fn set_origin(&mut self, system_name: &str, origin: u32, detail: &str) {
        let Ok(Some(mut row)) = self.registry.get(system_name) else {
            return;
        };
        if row.origin == origin {
            return;
        }
        row.origin = origin;
        let ok = self.registry.put(&row).is_ok();
        let record = event("provision", &Subject::of_row(&row), 0, ok, detail);
        self.audit.record(&record);
    }

    /// Install or upgrade the shipped `system_name`; `Ok(true)` for an upgrade.
    fn provision_one(&mut self, system_name: &str) -> Result<bool, Failure> {
        let path = format!("{}/{system_name}.lzp", fhs::SYSTEM_PACKAGES);
        let mut subject = Subject::none();
        subject.system_name = String::from(system_name);
        let outcome = store::read_package(&mut self.buffer, &path)
            .map_err(read_failure)
            .and_then(|()| {
                let bytes = core::mem::take(&mut self.buffer);
                let outcome = self.provision_bytes(&bytes, system_name, &mut subject);
                self.buffer = bytes;
                outcome
            });
        let detail = outcome.as_ref().err().map_or("", |f| f.text.as_str());
        let record = event("provision", &subject, 0, outcome.is_ok(), detail);
        self.audit.record(&record);
        outcome
    }

    fn provision_bytes(
        &mut self,
        bytes: &[u8],
        system_name: &str,
        subject: &mut Subject,
    ) -> Result<bool, Failure> {
        let assessed = assess(bytes).map_err(|info| fail(EINVAL, problem_text(&info.problems)))?;
        let info = &assessed.info;
        *subject = Subject {
            system_name: info.system_name.clone(),
            version: info.version.clone(),
            install_dir: info.install_dir.clone(),
            digest: info.digest.clone(),
        };
        if !info.problems.is_empty() {
            return Err(fail(EINVAL, problem_text(&info.problems)));
        }
        let expected = self.core.iter().find(|p| p.system_name == system_name);
        if info.system_name != system_name || expected.is_none_or(|p| p.digest != info.digest) {
            return Err(fail(
                EINVAL,
                "the package does not match the image's package index",
            ));
        }
        let previous = self
            .registry
            .get(system_name)
            .map_err(super::handlers::registry_down)?;
        let upgrade = previous.is_some();
        self.install_package(&assessed.package, subject, previous, wire::ORIGIN_CORE)?;
        Ok(upgrade)
    }

    /// The shipped set and each archive's size: every `<system_name>.lzp` in
    /// `/system/packages`, described by the image's index or, for a file the
    /// index does not cover, by reading the archive.
    fn shipped_set(&mut self) -> (Vec<Shipped>, Vec<(String, usize)>, u64) {
        let entries = match files::list(fhs::SYSTEM_PACKAGES) {
            Ok(entries) => entries,
            // An image that ships no core packages.
            Err(ENOENT) => return (Vec::new(), Vec::new(), 0),
            // Any other failure hides every package: count it as one, so
            // the pass does not look like a clean one.
            Err(errno) => {
                sys::write_str(&format!(
                    "PKGD:PROVISION:LIST:FAIL {} errno {errno}\n",
                    fhs::SYSTEM_PACKAGES
                ));
                return (Vec::new(), Vec::new(), 1);
            }
        };
        let index = files::read_up_to(fhs::system::PACKAGES_INDEX, provision::MAX_INDEX)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|text| match provision::parse_index(&text) {
                Ok(index) => index,
                Err(error) => {
                    sys::write_str(&format!(
                        "PKGD:PROVISION:INDEX:FAIL line {}: {}\n",
                        error.line, error.reason
                    ));
                    Vec::new()
                }
            })
            .unwrap_or_default();
        let mut shipped = Vec::new();
        let mut sizes = Vec::new();
        let mut unreadable = 0;
        for entry in entries.iter().take(provision::MAX_CORE) {
            let Some(name) = provision::package_file_name(&entry.name) else {
                continue;
            };
            let described = match index.iter().find(|p| p.system_name == name) {
                Some(package) => Some(package.clone()),
                None => self.describe_archive(name),
            };
            match described {
                Some(package) => {
                    sizes.push((String::from(name), entry.size as usize));
                    shipped.push(package);
                }
                None => unreadable += 1,
            }
        }
        (shipped, sizes, unreadable)
    }

    /// Read a shipped archive the index does not describe.
    fn describe_archive(&mut self, name: &str) -> Option<Shipped> {
        let path = format!("{}/{name}.lzp", fhs::SYSTEM_PACKAGES);
        let mut bytes = core::mem::take(&mut self.buffer);
        let described = store::read_package(&mut bytes, &path)
            .ok()
            .and_then(|()| lazypkg::Package::open(&bytes).ok())
            .filter(|package| package.manifest().app.system_name == name)
            .map(|package| Shipped {
                system_name: String::from(name),
                version: package.manifest().app.version.clone(),
                digest: hex::encode(&package.digest()),
                autostart: package.manifest().entry.autostart,
            });
        self.buffer = bytes;
        if described.is_none() {
            sys::write_str(&format!(
                "PKGD:PROVISION:FAIL sn={name} reason=\"the archive cannot be opened\"\n"
            ));
        }
        described
    }
}

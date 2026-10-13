//! The label's rule set as the gate sees it: reloads, revocation, wildcard
//! rules, the audit trail, and a soak.

use super::*;
use crate::ipc::acl::ANY_INTERFACE;
use crate::ipc::topics::fnv1a32;
use filepolicy::{rule_id, Kind};

const CV: &str = "/home/user/Documents/cv.txt";

fn compile(rules: &[&str]) -> Result<Vec<u64>, String> {
    filepolicy::compile(rules)
        .map_err(|(i, e)| format!("rule {i} {:?}: {}", rules.get(i), e.message()))
}

/// A new rule set takes effect on the next call (the cache follows the
/// generation), a revoked label stays confined with nothing, and a label that
/// never was confined is not made so by being revoked.
pub fn reload_revoke_sticky() -> Result<(), String> {
    with_world(|| {
        let label = confine_as_user(RULES, &[])?;
        check!(opens(FONT, O_RDONLY), "font before the reload");
        check!(!opens(NOTES, O_RDONLY), "notes before the reload");

        // Replace the grants: the cached decision must not survive.
        load_ids(label, &compile(&["read:$HOME/Documents"])?)?;
        check!(!opens(FONT, O_RDONLY), "font after its grant was removed");
        check!(
            !opens(COOKIES, O_RDONLY),
            "profile after its grant was removed"
        );
        check!(opens(CV, O_RDONLY), "Documents after it was granted");
        check!(!opens(NOTES, O_RDONLY), "notes are still outside");

        // And back.
        load_ids(label, &compile(RULES)?)?;
        check!(opens(FONT, O_RDONLY), "font after the grant came back");
        check!(!opens(CV, O_RDONLY), "Documents after its grant went");

        // Revocation (an uninstall loads an empty list): nothing is granted
        // and the label does not become free.
        acl::load_label(label, &[]).map_err(|e| format!("{e:?}"))?;
        check!(
            confine::confined(label),
            "a revoked sandbox label became free"
        );
        for path in [FONT, CV, COOKIES, NOTES, "/home/user/.ssh/id_rsa"] {
            check!(!opens(path, O_RDONLY), "{path} after revocation");
        }
        check!(
            opens("/dev/null", O_RDONLY),
            "the baseline went with the rules"
        );

        // A label that never carried the marker is not confined by an empty
        // rule set (nor its revocation).
        let free = labels::intern("app:os.lazy.free").map_err(|e| format!("{e:?}"))?;
        load_ids(free, &[1, 2, 3])?;
        acl::load_label(free, &[]).map_err(|e| format!("{e:?}"))?;
        check!(
            !confine::confined(free),
            "an ordinary label became confined"
        );
        Ok(())
    })
}

/// Only an exact rule id grants: an allow-everything rule (`ANY_INTERFACE`)
/// grants no file, a denied id stays denied behind a later allow, and the
/// marker alone is a confined label with nothing.
pub fn wildcard_rules() -> Result<(), String> {
    with_world(|| {
        let label = labels::intern(LABEL).map_err(|e| format!("{e:?}"))?;
        credentials::set(
            task::KERNEL_TASK,
            Cred::new(USER_CRED.0, USER_CRED.1, 0, label, 0),
        );
        let marker = filepolicy::confine_id();
        let allow = |interface_id| Rule {
            actor: label,
            interface_id,
            method: ANY_METHOD,
            allow: true,
        };

        // The marker and an allow-everything rule.
        acl::load_label(label, &[allow(marker), allow(ANY_INTERFACE)])
            .map_err(|e| format!("{e:?}"))?;
        check!(confine::confined(label), "the marker did not confine");
        check!(!opens(FONT, O_RDONLY), "a wildcard rule granted a file");
        check!(
            !opens(NOTES, O_RDONLY),
            "a wildcard rule granted a private file"
        );

        // An explicit deny of an id ahead of its allow wins, as for any rule.
        let font_read = rule_id(Kind::Read, "/system/share/fonts");
        let deny = Rule {
            allow: false,
            ..allow(font_read)
        };
        acl::load_label(label, &[allow(marker), deny, allow(font_read)])
            .map_err(|e| format!("{e:?}"))?;
        check!(
            !opens(FONT, O_RDONLY),
            "a denied id was granted by a later rule"
        );

        // A rule for one method only decides nothing.
        let one_method = Rule {
            method: 7,
            ..allow(font_read)
        };
        acl::load_label(label, &[allow(marker), one_method]).map_err(|e| format!("{e:?}"))?;
        check!(!opens(FONT, O_RDONLY), "a one-method rule granted a file");

        // The exact allow works.
        acl::load_label(label, &[allow(marker), allow(font_read)]).map_err(|e| format!("{e:?}"))?;
        check!(opens(FONT, O_RDONLY), "the exact rule granted nothing");
        Ok(())
    })
}

/// Every refusal is an audit record of the label, the file scope and the
/// path's hash; allowed accesses leave none (a sandbox reading fonts all day
/// does not fill the ring).
pub fn audited() -> Result<(), String> {
    with_world(|| {
        let label = confine_as_user(RULES, &[])?;
        for _ in 0..5 {
            check!(opens(FONT, O_RDONLY), "font");
            check!(opens(COOKIES, O_RDWR), "cookies");
        }
        check!(
            audit::recent(8).is_empty(),
            "allowed accesses were audited: {:?}",
            audit::recent(8)
        );

        check!(!opens("/home/user/.ssh/id_rsa", O_RDONLY), "key");
        let events = audit::recent(4);
        check!(events.len() == 1, "one refusal, {} records", events.len());
        let event = events[0];
        check!(!event.allow, "the record says allowed");
        check!(
            event.label_id == label,
            "label {} != {label}",
            event.label_id
        );
        check!(event.uid == USER_CRED.0, "uid {}", event.uid);
        check!(
            event.interface_id == confine::FILE_SCOPE,
            "interface {:#x}",
            event.interface_id
        );
        check!(
            event.method == fnv1a32("/home/user/.ssh/id_rsa"),
            "method {:#x}",
            event.method
        );
        check!(
            event.reason_code == acl::reason::LABEL_DEFAULT_DENY,
            "reason {}",
            event.reason_code
        );

        // A refused spawn and socket are in the same trail, apart from files.
        let before = audit::recent(16).len();
        let _ = process::linux::dispatch_for_test(57, 0, 0, 0);
        let _ = process::linux::dispatch_for_test(41, 2, 1, 0);
        let events = audit::recent(16);
        check!(events.len() == before + 2, "spawn and socket records");
        check!(
            events[..2]
                .iter()
                .all(|event| !event.allow && event.interface_id == confine::PROCESS_SCOPE),
            "process records: {:?}",
            &events[..2]
        );
        Ok(())
    })
}

const CYCLES: usize = 3_000;

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct Usage {
    frames: usize,
    heap: usize,
    slots: usize,
}

fn usage() -> Usage {
    Usage {
        frames: mem::frame_stats().live() as usize,
        heap: crate::mem::slab::stats().live_bytes as usize
            + crate::mem::heap_stats().used as usize,
        slots: task::free_slots(),
    }
}

/// One cycle: grants used, refusals taken, a file created and removed, a
/// directory made and removed, a descriptor opened and closed.
fn cycle(index: usize) -> Result<(), String> {
    check!(opens(FONT, O_RDONLY), "font");
    check!(openat(NOTES, O_RDONLY) == EPERM, "notes");
    check!(stat("/home/user/.ssh") == EPERM, "stat .ssh");
    check!(stat("/home/user/.apps") == 0, "stat .apps");
    let file = format!("{COOKIES}.{}", index % 4);
    let fd = openat(&file, O_WRONLY | O_CREAT);
    check!(fd >= 0, "create {file}: {fd}");
    close(fd);
    check!(unlink(&file) == 0, "unlink {file}");
    let dir = "/home/user/.apps/os.lazy.webkit/work";
    check!(mkdir(dir) == 0, "mkdir");
    check!(rmdir(dir) == 0, "rmdir");
    check!(mkdir("/home/user/evil") == EPERM, "mkdir outside");
    check!(
        process::linux::dispatch_for_test(57, 0, 0, 0) as i64 == EPERM,
        "fork"
    );
    Ok(())
}

/// Thousands of allowed and refused calls and rule reloads leak no
/// descriptor, frame, heap or slot, grow no cache past the label table, and
/// every decision stays what the loaded rules say.
pub fn soak() -> Result<(), String> {
    with_world(|| {
        let label = confine_as_user(RULES, &[])?;
        let (rules_a, rules_b) = (compile(RULES)?, compile(&["read:$HOME/Documents"])?);
        for warm in 0..8 {
            cycle(warm)?;
        }
        let before = usage();
        for index in 0..CYCLES {
            cycle(index)?;
        }
        let after = usage();
        check!(
            after == before,
            "{CYCLES} cycles moved usage from {before:?} to {after:?}"
        );

        // Rule churn: two rule sets swapped many times, the decision checked
        // after each swap, the label table and the cache not growing.
        let labels_before = labels::count();
        for round in 0..CYCLES / 3 {
            let (rules, font, cv) = if round % 2 == 0 {
                (&rules_a, true, false)
            } else {
                (&rules_b, false, true)
            };
            load_ids(label, rules)?;
            check!(opens(FONT, O_RDONLY) == font, "round {round}: font");
            check!(opens(CV, O_RDONLY) == cv, "round {round}: cv");
        }
        check!(labels::count() == labels_before, "the label table grew");
        check!(
            acl::label_rule_count(label) == rules_b.len(),
            "rules were duplicated"
        );
        check!(
            acl::label_rules_total() == rules_b.len(),
            "another label gained rules"
        );
        let fds = task::harness::fd_table_len();
        for fd in 0..fds {
            check!(
                task::fd_kind(fd) == task::FdKind::Closed || fd < 3,
                "descriptor {fd} left open"
            );
        }
        Ok(())
    })
}

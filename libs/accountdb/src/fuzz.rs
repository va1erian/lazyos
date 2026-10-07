//! Byte-level fuzzing of the account database and its operations.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/accountdb.rs`) and the seeded tests below. Whatever
//! the bytes, [`parse`](crate::parse) must not panic and must fail closed;
//! what it accepts must satisfy every rule `accountsd` and `keyd` rely on,
//! round-trip through [`Db::to_text`], and produce views the passwd parser
//! accepts. Then a sequence of operations chosen by the input's bytes runs
//! against it (`accountsd`'s state machine without the transport): none may
//! break an invariant, and the last administrator is never lost.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::ToString;

use crate::ops::OpError;
use crate::{parse, valid_name, Db, LoadError, DB_MAX, MAX_USERS};

fn check_invariants(db: &Db) {
    assert!(db.users.len() <= MAX_USERS);
    let mut names = BTreeSet::new();
    let mut uids = BTreeSet::new();
    for user in &db.users {
        assert!(valid_name(&user.name));
        assert!(names.insert(user.name.clone()), "duplicate name");
        assert!(uids.insert(user.uid), "duplicate uid");
        assert_ne!(user.uid, 0, "an account with uid 0");
        for group in &user.groups {
            assert!(
                db.groups.iter().any(|g| &g.name == group),
                "undeclared group"
            );
        }
    }
    let gids: BTreeSet<u32> = db.groups.iter().map(|g| g.gid).collect();
    assert_eq!(gids.len(), db.groups.len(), "duplicate gid");
    // What it holds, written out, parses back to the same database.
    let text = db.to_text();
    if text.len() <= DB_MAX {
        assert_eq!(parse(text.as_bytes()).as_ref(), Ok(db));
    }
    // The world-readable view is a passwd file `accountsd`'s old parser and
    // the kernel's `/etc/passwd` accept.
    if !db.users.is_empty() {
        let view = db.passwd_view();
        if view.len() <= passwd::PASSWD_MAX {
            let rows = passwd::parse(view.as_bytes()).expect("the passwd view parses");
            assert_eq!(rows.len(), db.users.len());
        }
    }
}

/// Apply operations picked by `input`'s bytes to `db`, checking after each.
fn operate(mut db: Db, input: &[u8]) {
    for (step, byte) in input.iter().take(32).enumerate() {
        let admins = db.admins();
        let pick = |db: &Db| {
            db.users
                .get(usize::from(*byte) % db.users.len().max(1))
                .map(|user| user.name.clone())
        };
        let result = match byte % 4 {
            0 => db
                .create(&format!("u{step}x{byte}"), byte & 4 != 0, None)
                .map(|_| ()),
            1 => match pick(&db) {
                Some(name) => db.delete(&name).map(|_| ()),
                None => Err(OpError::NotFound),
            },
            2 => match pick(&db) {
                Some(name) => db.set_admin(&name, byte & 8 != 0),
                None => Err(OpError::NotFound),
            },
            _ => db.create("_system", false, None).map(|_| ()),
        };
        if admins > 0 {
            assert!(db.admins() > 0, "the last administrator was lost");
        }
        if result == Err(OpError::LastAdmin) {
            assert_eq!(db.admins(), admins);
        }
        check_invariants(&db);
    }
}

/// Parse `input`, check the invariants, then run operations. Never panics.
pub fn run(input: &[u8]) {
    let result = parse(input);
    if input.len() > DB_MAX {
        assert_eq!(result, Err(LoadError::Oversize(input.len())));
        return;
    }
    if core::str::from_utf8(input).is_err() {
        assert_eq!(result, Err(LoadError::NotText));
        return;
    }
    match result {
        Ok(db) => {
            check_invariants(&db);
            operate(db, input);
        }
        // The reason text is what `ACCOUNTS:LOAD:FAIL` prints.
        Err(error) => assert!(!error.to_string().is_empty()),
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};
    use std::vec::Vec;

    /// Replay every checked-in seed (`fuzz/seeds/accountdb`) and saved crash
    /// (`fuzz/regressions/accountdb`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("accountdb")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for accountdb");
        }
    }

    const SECRET: &str = "argon2id:19456:2:1:abababababababababababababababab:\
                          5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

    fn record(rng: &mut Rng) -> Vec<u8> {
        let names: [&[u8]; 6] = [b"admin", b"user", b"bob", b"Bad", b"_x", b""];
        let ids: [&[u8]; 6] = [b"0", b"10", b"1000", b"1001", b"4294967296", b"x"];
        let homes: [&[u8]; 4] = [b"/home/a", b"/", b"home/a", b"/a/../b"];
        let groups: [&[u8]; 5] = [b"", b"admin", b"admin,admin", b"nogroup", b"admin,wheel"];
        let secrets: [&[u8]; 4] = [b"!", SECRET.as_bytes(), b"argon2id:1:1:1:ab:cd", b""];
        let mut out = Vec::new();
        match rng.below(4) {
            0 => {
                out.extend(b"group:");
                out.extend(*rng.pick(&names));
                out.push(b':');
                out.extend(*rng.pick(&ids));
            }
            1 => {
                out.extend(b"next:");
                out.extend(*rng.pick(&ids));
            }
            _ => {
                out.extend(b"user:");
                for part in [
                    *rng.pick(&names),
                    *rng.pick(&ids),
                    *rng.pick(&ids),
                    *rng.pick(&homes),
                    b"sh".as_slice(),
                    *rng.pick(&groups),
                    *rng.pick(&secrets),
                ] {
                    out.extend(part);
                    out.push(b':');
                }
                out.pop();
            }
        }
        out
    }

    #[test]
    fn generated_databases_are_safe() {
        for_seeds("accountdb::generated_databases_are_safe", |_, rng| {
            let mut file = Vec::new();
            for _ in 0..rng.range(0, 10) {
                file.extend(record(rng));
                file.extend(*rng.pick(&[&b"\n"[..], b"\r\n", b"\n#c\n"]));
            }
            run(&file);
            rng.flip_bits(&mut file, 2);
            run(&file);
        });
    }

    #[test]
    fn raw_noise_is_safe() {
        for_seeds("accountdb::raw_noise_is_safe", |_, rng| {
            let len = rng.range(0, 512) as usize;
            run(&rng.bytes(len));
        });
    }

    #[test]
    fn a_valid_database_survives_mutation() {
        let good = format!(
            "next:1002\ngroup:admin:10\nuser:admin:1001:1001:/home/admin:sh:admin:{SECRET}\n\
             user:user:1000:1000:/home/user:sh::!\n"
        );
        for_seeds("accountdb::a_valid_database_survives_mutation", |_, rng| {
            let mut data = good.clone().into_bytes();
            let flips = rng.range(0, 6) as usize;
            rng.flip_bits(&mut data, flips);
            run(&data);
        });
    }
}

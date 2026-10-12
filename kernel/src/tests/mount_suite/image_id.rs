//! `image.id=` in `lazyos.cfg` (docs/netboot-plan.md N1): the build's name,
//! parsed and kept by `boot_image`, never able to cost the boot its mount.

use super::*;
use crate::fs::boot_image::{self, Refused};
use crate::fs::bootcfg::parse;
use crate::fs::mounts;

const ID: &str = "20261012-093000-0123abcd";

fn cfg(extra: &str) -> String {
    format!("root=UUID={}\nhome=LABEL=home\n{extra}", uuid(0xA1).1)
}

/// Mount the rig whose boot volume carries `text` as `lazyos.cfg`.
fn mount(text: &str) -> mounts::Tables {
    let rig = rig(Some(text));
    mounts::build(&[rig.boot, rig.root, rig.home])
}

fn points(tables: &mounts::Tables) -> Vec<String> {
    tables
        .native
        .mounts()
        .into_iter()
        .map(|(point, _)| point)
        .collect()
}

const CONFIGURED: [&str; 5] = ["/", "/boot", "/transient", "/tmp", "/home"];

/// A valid id is accepted by the rule, skipped by the config parser (which
/// still reads the root), kept by `apply_config` and shown by `/proc/version`.
pub fn valid_id_is_parsed() -> Result<(), String> {
    boot_image::reset_for_test();
    check!(boot_image::validate(ID) == Ok(ID), "{ID} refused");
    let text = cfg(&format!("image.id={ID}\n"));
    let parsed = parse(&text).map_err(|e| format!("{e}"))?;
    check!(
        parsed.root == Some(uuid(0xA1).0),
        "root lost: {:?}",
        parsed.root
    );
    check!(boot_image::get().is_none(), "parse alone stored an id");

    boot_image::apply_config(&text);
    check!(
        boot_image::get().as_deref() == Some(ID),
        "stored {:?}",
        boot_image::get()
    );
    // The mount reads the config twice: the line is printed once.
    boot_image::apply_config(&text);
    check!(
        boot_image::announced_for_test() == 1,
        "BOOT:IMAGE printed {} times",
        boot_image::announced_for_test()
    );
    // Spaces around `=` are fine, as for every other key; the last line wins.
    boot_image::apply_config(&format!("image.id = first\nimage.id =  {ID} \n"));
    check!(boot_image::get().as_deref() == Some(ID), "last line lost");
    let proc = crate::process::linux::proc_file_for_test("/proc/version")
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .ok_or("no /proc/version")?;
    check!(
        proc.trim_end().ends_with(&format!(" image {ID}")),
        "/proc/version: {proc:?}"
    );
    boot_image::reset_for_test();
    Ok(())
}

/// A missing line logs and stores nothing.
pub fn missing_line_changes_nothing() -> Result<(), String> {
    boot_image::reset_for_test();
    boot_image::apply_config(&cfg(""));
    boot_image::apply_config("# image.id=commented\nimage_id=wrong-key\n");
    check!(
        boot_image::get().is_none(),
        "invented {:?}",
        boot_image::get()
    );
    let tables = mount(&cfg(""));
    check!(tables.mounted && points(&tables) == CONFIGURED, "layout");
    Ok(())
}

/// A bad id is refused and the layout still mounts, whatever else the file
/// says; a good id stored before is not replaced by it.
pub fn bad_id_is_refused_without_touching_the_mount() -> Result<(), String> {
    boot_image::reset_for_test();
    let bad = [
        "image.id=has space",
        "image.id=semi;colon",
        "image.id=slash/slash",
        "image.id=caf\u{e9}",
        "image.id=",
        "image.id=a=b",
        "image.unknown=1",
    ];
    for line in bad {
        let text = cfg(&format!("{line}\nroot_flags=noexec\n"));
        check!(
            parse(&text).is_ok(),
            "{line:?}: the parser refused the file"
        );
        let tables = mount(&text);
        check!(tables.mounted, "{line:?}: nothing mounted");
        check!(
            points(&tables) == CONFIGURED,
            "{line:?}: {:?}",
            points(&tables)
        );
        check!(
            tables.native.mount_flags("/etc/x").noexec,
            "{line:?}: root flags lost"
        );
        check!(boot_image::get().is_none(), "{line:?} stored an id");
    }
    boot_image::apply_config(&format!("image.id={ID}\n"));
    boot_image::apply_config("image.id=bad id\n");
    check!(
        boot_image::get().as_deref() == Some(ID),
        "a bad line replaced a good id"
    );
    boot_image::reset_for_test();
    Ok(())
}

/// The length rule: 64 bytes pass, 65 and 0 are refused.
pub fn overlong_id_is_refused() -> Result<(), String> {
    boot_image::reset_for_test();
    let longest = "a".repeat(boot_image::MAX_LEN);
    check!(boot_image::validate(&longest).is_ok(), "64 bytes refused");
    check!(
        boot_image::validate(&format!("{longest}a")) == Err(Refused::TooLong),
        "65 bytes accepted"
    );
    check!(
        boot_image::validate("") == Err(Refused::Empty),
        "empty accepted"
    );
    check!(
        boot_image::validate("a b") == Err(Refused::BadCharacter),
        "space accepted"
    );

    let text = cfg(&format!("image.id={longest}a\n"));
    check!(parse(&text).is_ok(), "an over-long id failed the parse");
    let tables = mount(&text);
    check!(tables.mounted && points(&tables) == CONFIGURED, "layout");
    check!(boot_image::get().is_none(), "an over-long id was stored");

    boot_image::apply_config(&format!("image.id={longest}\n"));
    check!(boot_image::get() == Some(longest), "64 bytes not stored");
    boot_image::reset_for_test();
    Ok(())
}

/// Many random lines: `apply_config` agrees with the rule on every one, and
/// the config parser never fails on them (they must not cost a mount).
pub fn soak_random_ids() -> Result<(), String> {
    const ALPHABET: &[u8] = b"abcXYZ019._- /=;\xc3\xa9\t";
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    boot_image::reset_for_test();
    let (mut accepted, mut refused) = (0u32, 0u32);
    for round in 0..4000 {
        let len = (next() % 70) as usize;
        let id: String = (0..len)
            .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
            .collect::<Vec<u8>>()
            .into_iter()
            .map(char::from)
            .collect();
        boot_image::reset_for_test();
        let text = format!("root=UUID={}\nimage.id={id}\n", uuid(1).1);
        check!(
            parse(&text).is_ok(),
            "round {round}: parse failed for {id:?}"
        );
        boot_image::apply_config(&text);
        let want = boot_image::validate(id.trim()).ok().map(String::from);
        check!(
            boot_image::get() == want,
            "round {round}: {id:?} stored {:?}, rule says {want:?}",
            boot_image::get()
        );
        if want.is_some() {
            accepted += 1;
        } else {
            refused += 1;
        }
    }
    check!(
        accepted > 0 && refused > 0,
        "{accepted} accepted, {refused} refused"
    );
    boot_image::reset_for_test();
    Ok(())
}

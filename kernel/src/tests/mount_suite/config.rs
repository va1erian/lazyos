//! `lazyos.cfg` parsing, including hostile input.

use super::*;
use crate::fs::bootcfg::{self, parse, parse_bytes, CfgError, VolumeId};
use crate::fs::vfs::MountFlags;

/// Every directive, with comments, blank lines and an unknown key mixed in.
pub fn parses_every_key() -> Result<(), String> {
    let (root, root_text) = uuid(0xAB);
    let (home, home_text) = uuid(0x12);
    let text = format!(
        "# the boot config\n\nroot=UUID={root_text}\nhome=LABEL=home\n\
         root_flags=noexec,nosuid\nfuture_key=1\nhome_flags = ro \n"
    );
    let cfg = parse(&text).map_err(|e| format!("{e}"))?;
    check!(cfg.root == Some(root), "root {:?}", cfg.root);
    let mut label = [0u8; 16];
    label[..4].copy_from_slice(b"home");
    check!(
        cfg.home == Some(VolumeId::Label(label)),
        "home {:?}",
        cfg.home
    );
    check!(
        cfg.root_flags
            == MountFlags {
                ro: false,
                noexec: true,
                nosuid: true
            },
        "root flags {:?}",
        cfg.root_flags
    );
    check!(cfg.home_flags.ro && !cfg.home_flags.noexec, "home flags");
    let by_uuid = parse(&format!("home=UUID={home_text}")).map_err(|e| format!("{e}"))?;
    check!(by_uuid.home == Some(VolumeId::Uuid(home)), "home by uuid");
    check!(
        parse("").map_err(|e| format!("{e}"))? == Default::default(),
        "empty file"
    );
    Ok(())
}

/// Each hostile input is refused whole, with the right reason.
pub fn hostile_inputs() -> Result<(), String> {
    let (_, text) = uuid(1);
    let big = "#".repeat(bootcfg::MAX_BYTES + 1);
    check!(parse(&big) == Err(CfgError::TooLarge), "4 KiB + 1 accepted");
    let exact = "#".repeat(bootcfg::MAX_BYTES);
    check!(parse(&exact).is_ok(), "exactly 4 KiB refused");
    check!(
        parse_bytes(&vec![b'#'; bootcfg::MAX_BYTES + 1]) == Err(CfgError::TooLarge),
        "bytes: 4 KiB + 1 accepted"
    );
    check!(parse("root=UUID=\0").is_err(), "NUL accepted");
    check!(
        parse_bytes(b"root=\xFF\xFE") == Err(CfgError::NotUtf8),
        "invalid UTF-8 accepted"
    );
    let dup = format!("root=UUID={text}\nroot=UUID={text}\n");
    check!(
        matches!(parse(&dup), Err(CfgError::Duplicate(_))),
        "duplicate key accepted"
    );
    check!(
        matches!(
            parse(&format!("root=UUID={text}0")),
            Err(CfgError::BadValue(_))
        ),
        "a 37-character uuid accepted"
    );
    check!(
        matches!(
            parse(&format!("root=UUID={}", &text[..35])),
            Err(CfgError::BadValue(_))
        ),
        "a 35-character uuid accepted"
    );
    check!(
        matches!(
            parse(&format!("root=UUID={}", text.replace('-', "x"))),
            Err(CfgError::BadValue(_))
        ),
        "uuid without dashes accepted"
    );
    check!(
        matches!(
            parse(&format!("root=UUID={}", text.replace('0', "g"))),
            Err(CfgError::BadValue(_))
        ),
        "non-hex uuid accepted"
    );
    check!(
        matches!(parse("root=/dev/sda"), Err(CfgError::BadValue(_))),
        "bare device accepted"
    );
    check!(
        matches!(parse("root_flags=ro,exec"), Err(CfgError::BadValue(_))),
        "unknown flag"
    );
    check!(
        matches!(parse("root_flags=ro,,noexec"), Err(CfgError::BadValue(_))),
        "empty flag"
    );
    check!(
        matches!(parse("home=LABEL="), Err(CfgError::BadValue(_))),
        "empty label"
    );
    check!(
        matches!(
            parse("home=LABEL=this-label-is-too-long"),
            Err(CfgError::BadValue(_))
        ),
        "17+ byte label accepted"
    );
    check!(
        matches!(parse("just words"), Err(CfgError::Malformed(1))),
        "line without ="
    );
    Ok(())
}

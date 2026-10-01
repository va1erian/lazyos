//! `append_file` (syscall 28), the chunked write the package installer uses for
//! files bigger than one `write_file`. Split out of `fsops_suite.rs`.

use super::*;

const APPEND_FILE: u64 = 28;

/// `append_file` (28): the chunked write the package installer uses for files
/// bigger than one `write_file`.
fn append(path: &str, data: &[u8]) -> u64 {
    let path = cstr(path);
    call(
        APPEND_FILE,
        path.as_ptr() as u64,
        data.as_ptr() as u64,
        data.len() as u64,
    )
}

/// Appending creates, extends in order, refuses a directory and a bad pointer,
/// and a file larger than `MAX_WRITE` round-trips through chunks.
pub(super) fn append_file_semantics() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/ap/a", "/tmp/ap/big", "/tmp/ap"]);
    check!(path_call(MKDIR, "/tmp/ap") == 0, "mkdir");
    check!(append("/tmp/ap/a", b"one") == 3, "append did not create");
    check!(append("/tmp/ap/a", b"-two") == 4, "second append");
    check!(append("/tmp/ap/a", b"") == 0, "an empty append");
    check!(
        slurp("/tmp/ap/a").as_deref() == Some(&b"one-two"[..]),
        "appended bytes out of order"
    );
    // A replace after appends starts over, like any write_file.
    check!(put("/tmp/ap/a", b"x") == 1, "write_file after append");
    check!(append("/tmp/ap/a", b"y") == 1, "append after write_file");
    check!(
        slurp("/tmp/ap/a").as_deref() == Some(&b"xy"[..]),
        "write_file then append"
    );
    check!(
        append("/tmp/ap", b"d") == failed(21),
        "appended to a directory"
    );
    check!(
        append("/tmp/nodir/x", b"d") == failed(ENOENT),
        "appended below a missing directory"
    );
    let huge = process::fsops::MAX_WRITE + 1;
    let path = cstr("/tmp/ap/a");
    check!(
        call(
            APPEND_FILE,
            path.as_ptr() as u64,
            path.as_ptr() as u64,
            huge
        ) == failed(ENOSPC),
        "an oversized append was accepted"
    );
    // A file larger than one write: 3 chunks of MAX_WRITE / 2 + 5.
    let chunk = (process::fsops::MAX_WRITE as usize) / 2 + 5;
    let mut expected = Vec::new();
    for index in 0..3u8 {
        let part = vec![index + 1; chunk];
        let wrote = if index == 0 {
            put("/tmp/ap/big", &part)
        } else {
            append("/tmp/ap/big", &part)
        };
        check!(wrote == chunk as u64, "chunk {index}: wrote {wrote}");
        expected.extend_from_slice(&part);
    }
    check!(
        stat("/tmp/ap/big") == Ok((expected.len() as u64, 0)),
        "size after chunks: {:?}",
        stat("/tmp/ap/big")
    );
    let path = cstr("/tmp/ap/big");
    let mut back = vec![0u8; expected.len()];
    let n = call(
        3,
        path.as_ptr() as u64,
        back.as_mut_ptr() as u64,
        back.len() as u64,
    );
    check!(
        n == expected.len() as u64 && back == expected,
        "chunked file differs"
    );
    // A bad data pointer neither creates nor grows a file.
    let secret = vec![0xA5u8; 8];
    strict(|| -> Result<(), String> {
        let fresh_path = cstr("/tmp/ap/ghost");
        check!(
            call(
                APPEND_FILE,
                fresh_path.as_ptr() as u64,
                secret.as_ptr() as u64,
                8
            ) == failed(EFAULT),
            "append read kernel memory"
        );
        Ok(())
    })?;
    check!(
        stat("/tmp/ap/ghost") == Err(failed(ENOENT)),
        "a refused append created a file"
    );
    Ok(())
}

/// Soak: many appends to one growing file and many generations of fresh files
/// leak neither frames nor entries, and the bytes stay in order.
pub(super) fn soak_append_churn() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/apsoak/log", "/tmp/apsoak"]);
    check!(path_call(MKDIR, "/tmp/apsoak") == 0, "mkdir");
    let before = mem::frame_stats().live();
    for generation in 0..40u32 {
        let mut expected = Vec::new();
        for piece in 0..50u32 {
            let part =
                vec![(generation + piece) as u8; 1 + ((generation * 7 + piece) % 200) as usize];
            check!(
                append("/tmp/apsoak/log", &part) == part.len() as u64,
                "gen {generation} piece {piece}: append"
            );
            expected.extend_from_slice(&part);
        }
        let path = cstr("/tmp/apsoak/log");
        let mut back = vec![0u8; expected.len() + 16];
        let n = call(
            3,
            path.as_ptr() as u64,
            back.as_mut_ptr() as u64,
            back.len() as u64,
        );
        check!(
            n == expected.len() as u64 && back[..n as usize] == expected[..],
            "gen {generation}: content differs"
        );
        check!(
            path_call(UNLINK, "/tmp/apsoak/log") == 0,
            "gen {generation}: unlink"
        );
    }
    check!(
        list("/tmp/apsoak") == Ok(String::new()),
        "entries leaked: {:?}",
        list("/tmp/apsoak")
    );
    check!(path_call(UNLINK, "/tmp/apsoak") == 0, "rmdir");
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}

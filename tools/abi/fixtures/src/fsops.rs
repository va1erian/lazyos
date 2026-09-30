//! `fsops` — the filesystem operations a file manager and an editor need
//! (`std::fs` through the Linux shim): `read_dir` with `file_type` (getdents64
//! `d_type`), `symlink_metadata` (lstat), `create_dir`, file create/write,
//! `rename` (also over an existing file, the atomic-save pattern),
//! `remove_file`, `remove_dir` and `remove_dir_all`.
//!
//! It runs the same script on every writable base the image has and reports
//! each finished base as `ABI:fsops:ROUND:<base>` (the bench insists on
//! `/tmp`, the FAT root `/`, and `/data` when a disk is attached). A step that
//! fails prints `ABI:fsops:GAP:<base>:<step>:<error>` and fails the fixture, so
//! a shim gap is never silent.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

const NAME: &str = "fsops";

fn step<T>(base: &str, name: &str, result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|error| format!("{base}:{name}:{error}"))
}

fn expect(base: &str, name: &str, ok: bool, what: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(format!("{base}:{name}:{what}"))
    }
}

/// Write `contents` to `path` (create or truncate) and flush it to the file.
fn put(path: &str, contents: &str) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

/// `base` is a directory without a trailing slash (`""` is the FAT root).
/// Names are 8.3-safe uppercase so the FAT volume accepts them too.
fn round(base: &str) -> Result<(), String> {
    let dir = format!("{base}/FSOPSD");
    let sub = format!("{dir}/SUB");
    let file_a = format!("{dir}/A.TXT");
    let file_b = format!("{dir}/B.TXT");
    let file_c = format!("{dir}/C.TXT");
    let inner = format!("{sub}/IN.TXT");

    // A previous run may have left the tree (ext2 cannot always clean up).
    let _ = fs::remove_dir_all(&dir);
    step(base, "create_dir", fs::create_dir(&dir))?;
    step(base, "create_dir_nested", fs::create_dir(&sub))?;
    step(base, "create_file", put(&file_a, "alpha"))?;
    step(base, "create_file_nested", put(&inner, "inner"))?;

    // read_dir + file_type: getdents64's d_type, no follow-up stat needed.
    let mut seen = Vec::new();
    for entry in step(base, "read_dir", fs::read_dir(&dir))? {
        let entry = step(base, "read_dir_entry", entry)?;
        let kind = step(base, "file_type", entry.file_type())?;
        seen.push((entry.file_name().to_string_lossy().to_string(), kind.is_dir()));
    }
    seen.sort();
    expect(
        base,
        "read_dir_d_type",
        seen == [("A.TXT".to_string(), false), ("SUB".to_string(), true)],
        &format!("listed {seen:?}"),
    )?;

    // lstat: type and size without following anything.
    let meta = step(base, "lstat_file", fs::symlink_metadata(&file_a))?;
    expect(
        base,
        "lstat_file",
        meta.is_file() && !meta.is_dir() && meta.len() == 5,
        &format!("file={} dir={} len={}", meta.is_file(), meta.is_dir(), meta.len()),
    )?;
    let meta = step(base, "lstat_dir", fs::symlink_metadata(&sub))?;
    expect(base, "lstat_dir", meta.is_dir() && !meta.is_file(), "not a directory")?;
    expect(
        base,
        "lstat_missing",
        fs::symlink_metadata(format!("{dir}/NOPE.TXT")).is_err(),
        "a missing name has metadata",
    )?;

    // rename to a new name.
    step(base, "rename", fs::rename(&file_a, &file_b))?;
    expect(base, "rename_old_gone", fs::metadata(&file_a).is_err(), "old name survived")?;
    expect(
        base,
        "rename_content",
        step(base, "read_renamed", fs::read_to_string(&file_b))? == "alpha",
        "content changed",
    )?;

    // Atomic save: write a temp file, rename it over the existing target.
    step(base, "create_temp", put(&file_c, "beta-longer"))?;
    step(base, "rename_over_existing", fs::rename(&file_c, &file_b))?;
    expect(base, "rename_over_temp_gone", fs::metadata(&file_c).is_err(), "temp survived")?;
    expect(
        base,
        "rename_over_content",
        step(base, "read_replaced", fs::read_to_string(&file_b))? == "beta-longer",
        "target keeps old content",
    )?;

    // Truncating rewrite of an existing file shrinks it.
    step(base, "rewrite", put(&file_b, "z"))?;
    expect(
        base,
        "rewrite_shrinks",
        step(base, "read_rewritten", fs::read_to_string(&file_b))? == "z",
        "stale tail after truncate",
    )?;

    // Removal: file, empty dir, then the whole tree.
    step(base, "remove_file", fs::remove_file(&file_b))?;
    expect(base, "remove_file_gone", !Path::new(&file_b).exists(), "file survived")?;
    expect(
        base,
        "remove_dir_nonempty",
        fs::remove_dir(&sub).is_err(),
        "removed a non-empty directory",
    )?;
    step(base, "remove_file_nested", fs::remove_file(&inner))?;
    step(base, "remove_dir", fs::remove_dir(&sub))?;
    step(base, "create_again", put(&file_c, "again"))?;
    step(base, "remove_dir_all", fs::remove_dir_all(&dir))?;
    expect(base, "remove_dir_all_gone", !Path::new(&dir).exists(), "tree survived")?;

    println!("ABI:{NAME}:ROUND:{}", if base.is_empty() { "/" } else { base });
    Ok(())
}

/// Whether a data volume is mounted (`/proc/mounts` lists `/data`).
fn has_data_mount() -> bool {
    fs::read_to_string("/proc/mounts")
        .map(|mounts| mounts.lines().any(|line| line.split_whitespace().nth(1) == Some("/data")))
        .unwrap_or(false)
}

fn main() {
    let mut bases = vec!["/tmp", ""];
    if has_data_mount() {
        bases.push("/data");
    }
    let mut failed = false;
    for base in bases {
        if let Err(gap) = round(base) {
            println!("ABI:{NAME}:GAP:{gap}");
            failed = true;
        }
    }
    if failed {
        common::fail(NAME, "see the GAP lines");
    }
    common::pass(NAME);
}

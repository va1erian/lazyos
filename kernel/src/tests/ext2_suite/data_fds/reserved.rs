//! The `.unlinked-` prefix is reserved for the kernel's own parking of
//! unlinked-but-open files (issue #346): nothing else may create a name in it.

use super::*;

/// `rename(from, to)`.
fn rename(from: &str, to: &str) -> u64 {
    let (from, to) = (cstr(from), cstr(to));
    syscall(SYS_RENAME, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0)
}

/// `open(O_CREAT)`, `mkdir` and `rename` onto a reserved name answer `EINVAL`
/// and change nothing; near misses are ordinary names; and the kernel's own
/// parking of an open file still works.
pub fn reserved_prefix_is_refused() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(
        open("/data/.unlinked-1", O_CREAT | O_RDWR) == errno(EINVAL),
        "O_CREAT of a reserved name"
    );
    check!(
        path_call(SYS_MKDIR, "/data/.unlinked-2", 0o755) == errno(EINVAL),
        "mkdir of a reserved name"
    );
    check!(
        path_call(SYS_MKDIR, "/data/.unlinked-2/", 0o755) == errno(EINVAL),
        "mkdir of a reserved name with a trailing slash"
    );
    put("/data/a", b"A")?;
    check!(
        rename("/data/a", "/data/.unlinked-3") == errno(EINVAL),
        "rename onto a reserved name"
    );
    check!(slurp("/data/a")? == b"A", "a refused rename moved the file");
    check!(data_names()? == ["a"], "a refused call left a name behind");

    // Near misses are ordinary names.
    for name in ["/data/.unlinked", "/data/x.unlinked-1", "/data/.unlink-1"] {
        put(name, b"ok")?;
        check!(slurp(name)? == b"ok", "{name} is not an ordinary name");
    }

    // The kernel's own parking is not a user create.
    let fd = open("/data/a", O_RDWR);
    check!(
        path_call(SYS_UNLINK, "/data/a", 0) == 0,
        "unlink while open"
    );
    check!(read(fd, 1) == Ok(b"A".to_vec()), "the parked file is gone");
    close(fd);
    check!(
        data_names()?
            .iter()
            .all(|name| !name.starts_with(".unlinked-")),
        "the parked entry outlived the close"
    );
    data.check_clean()
}

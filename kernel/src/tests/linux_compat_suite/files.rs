//! Links, the fabricated `/etc` and `/proc` files, and the newer descriptor
//! and path calls (`dup3`, `renameat2`, `faccessat`).

use super::*;

const READLINK: u64 = 89;
const READLINKAT: u64 = 267;
const LINK: u64 = 86;
const SYMLINK: u64 = 88;
const DUP3: u64 = 292;
const RENAMEAT2: u64 = 316;
const FACCESSAT: u64 = 269;
const AT_FDCWD: u64 = -100i64 as u64;

fn readlink(path: &str) -> Result<String, u64> {
    let name = cpath(path);
    let mut buf = [0u8; 256];
    let n = sys(
        READLINK,
        &[name.as_ptr() as u64, buf.as_mut_ptr() as u64, 256],
    );
    if (n as i64) < 0 {
        return Err(n);
    }
    Ok(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// `/proc/self/{exe,cwd,fd/N}` resolve; a plain file is `EINVAL` (what
/// `realpath` relies on), a missing one `ENOENT`; creating links is `EPERM`.
pub fn links_and_readlink() -> Result<(), String> {
    fresh()?;
    check!(
        readlink("/proc/self/exe") == Ok(String::from(fhs::bin::BUSYBOX)),
        "exe {:?}",
        readlink("/proc/self/exe")
    );
    check!(readlink("/proc/self/cwd").is_ok(), "cwd");
    let (r, w) = pipe()?;
    let target = readlink(&format!("/proc/self/fd/{r}")).map_err(|e| format!("fd link {e:#x}"))?;
    check!(target.starts_with("pipe:["), "fd target {target}");
    check!(
        readlink("/proc/self/fd/0") == Ok(String::from("/dev/tty")),
        "fd 0"
    );
    check!(
        readlink("/proc/self/fd/13") == Err(ENOENT),
        "a closed fd resolved"
    );
    check!(
        readlink("/tmp") == Err(EINVAL),
        "a directory is not a link: {:?}",
        readlink("/tmp")
    );
    check!(
        readlink("/tmp/no-such-entry") == Err(ENOENT),
        "a missing path"
    );
    let name = cpath("/proc/self/exe");
    let mut small = [0u8; 4];
    let n = sys(
        READLINKAT,
        &[AT_FDCWD, name.as_ptr() as u64, small.as_mut_ptr() as u64, 4],
    );
    check!(n == 4 && &small == b"/sys", "truncation: {n} {small:?}");
    check!(
        sys(
            READLINK,
            &[name.as_ptr() as u64, small.as_mut_ptr() as u64, 0]
        ) == EINVAL,
        "size 0"
    );
    let (a, b) = (cpath("/tmp/a"), cpath("/tmp/b"));
    check!(
        sys(LINK, &[a.as_ptr() as u64, b.as_ptr() as u64]) == EPERM,
        "link"
    );
    check!(
        sys(SYMLINK, &[a.as_ptr() as u64, b.as_ptr() as u64]) == EPERM,
        "symlink"
    );
    check!(
        sys_checked(SYMLINK, &[8, b.as_ptr() as u64]) == EFAULT,
        "a bad target pointer"
    );
    sys(3, &[r]);
    sys(3, &[w]);
    Ok(())
}

/// The account database never carries a secret; the resolver and host files
/// exist; `/etc` lists them.
pub fn etc_files() -> Result<(), String> {
    fresh()?;
    let source =
        "admin:0:0:nimda:/home/admin:sh\nbroken line\nuser:1000:1000:lazy:/home/user:/bin/ash\n";
    let passwd = process::linux::render_passwd_for_test(source);
    check!(
        passwd
            == "admin:x:0:0:admin:/home/admin:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/ash\n",
        "passwd rendered as {passwd:?}"
    );
    check!(
        !passwd.contains("nimda") && !passwd.contains("lazy:"),
        "a secret leaked"
    );
    let group = process::linux::render_group_for_test(source, "");
    check!(
        group == "admin:x:0:admin\nuser:x:1000:user\n",
        "group rendered as {group:?}"
    );
    // The group view adds the `admin` group (U1) and wins a clash: a primary
    // group of the same name or gid yields, a malformed line is skipped.
    let view = "admin:10:admin,user\nbad line\nstaff:1000:x\n";
    let group = process::linux::render_group_for_test(source, view);
    check!(
        group == "admin:x:10:admin,user\nstaff:x:1000:x\n",
        "group with the view rendered as {group:?}"
    );
    // `/etc/resolv.conf` and the CA bundle exist only with their backing
    // files (`etcmap`); the host table always has `localhost`.
    check!(fabricated("/etc/hosts")?.contains("127.0.0.1"), "hosts");
    let live = fabricated("/etc/passwd")?;
    check!(
        live.lines().all(|line| line.split(':').nth(1) == Some("x")),
        "live passwd {live:?}"
    );
    // stat and open through the syscall layer, read-only.
    let name = cpath("/etc/hosts");
    let fd = sys(2, &[name.as_ptr() as u64, 0, 0]);
    check!((fd as i64) >= 3, "open /etc/hosts: {fd:#x}");
    sys(3, &[fd]);
    check!(
        sys(2, &[name.as_ptr() as u64, 1, 0]) == neg(30),
        "write-open of /etc/hosts not EROFS"
    );
    let dir = cpath("/etc");
    let dfd = sys(2, &[dir.as_ptr() as u64, 0o200000, 0]);
    check!((dfd as i64) >= 3, "open /etc: {dfd:#x}");
    let mut buf = [0u8; 1024];
    let n = sys(217, &[dfd, buf.as_mut_ptr() as u64, 1024]);
    check!(
        (n as i64) > 0 && buf[..n as usize].windows(9).any(|w| w == b"os-releas"),
        "getdents /etc: {n:#x}"
    );
    sys(3, &[dfd]);
    Ok(())
}

/// The machine and process files carry real values.
pub fn proc_files() -> Result<(), String> {
    fresh()?;
    let meminfo = fabricated("/proc/meminfo")?;
    let total_kb = crate::mem::frame_stats().total as u64 * 4;
    check!(
        meminfo.contains(&format!("MemTotal:       {total_kb:>8} kB")),
        "meminfo {meminfo:?}"
    );
    check!(
        fabricated("/proc/cpuinfo")?.contains("processor\t: 0"),
        "cpuinfo"
    );
    let uptime = fabricated("/proc/uptime")?;
    let secs: u64 = uptime
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or("uptime")?;
    check!(secs == task::ticks() / 100, "uptime {uptime:?}");
    let stat = fabricated("/proc/self/stat")?;
    check!(
        stat.starts_with(&format!("{} (", task::current())),
        "stat {stat:?}"
    );
    check!(
        stat.split(' ').count() == 52,
        "stat has {} fields",
        stat.split(' ').count()
    );
    let status = fabricated("/proc/self/status")?;
    check!(
        status.contains("Pid:\t0") && status.contains("Uid:"),
        "status {status:?}"
    );
    check!(fabricated("/proc/loadavg")?.starts_with("0.00"), "loadavg");
    check!(fabricated("/proc/stat")?.starts_with("cpu "), "/proc/stat");
    Ok(())
}

/// `dup3` refuses `old == new` and takes `O_CLOEXEC`; `renameat2` honours
/// `RENAME_NOREPLACE`; `faccessat` takes a directory descriptor.
pub fn dup3_renameat2_faccessat() -> Result<(), String> {
    fresh()?;
    let (r, w) = pipe()?;
    check!(sys(DUP3, &[r, r, 0]) == EINVAL, "dup3 old == new");
    check!(sys(DUP3, &[r, 10, 0o2000000]) == 10, "dup3");
    check!(task::fd_cloexec(10), "dup3 dropped O_CLOEXEC");
    check!(sys(DUP3, &[r, 11, 1]) == EINVAL, "a bad dup3 flag");
    let (a, b) = (cpath("/tmp/compat-a"), cpath("/tmp/compat-b"));
    for name in [&a, &b] {
        let fd = sys(2, &[name.as_ptr() as u64, 0o101, 0o644]);
        check!((fd as i64) >= 0, "create {fd:#x}");
        sys(3, &[fd]);
    }
    let args = |flags: u64| {
        [
            AT_FDCWD,
            a.as_ptr() as u64,
            AT_FDCWD,
            b.as_ptr() as u64,
            flags,
        ]
    };
    check!(
        sys(RENAMEAT2, &args(1)) == EEXIST,
        "NOREPLACE over an existing file"
    );
    check!(sys(RENAMEAT2, &args(2)) == EINVAL, "EXCHANGE accepted");
    check!(sys(RENAMEAT2, &args(0)) == 0, "a plain renameat2");
    check!(
        sys(FACCESSAT, &[AT_FDCWD, b.as_ptr() as u64, 4]) == 0,
        "faccessat R_OK"
    );
    check!(
        sys(FACCESSAT, &[AT_FDCWD, a.as_ptr() as u64, 0]) == ENOENT,
        "faccessat on the old name"
    );
    sys(87, &[b.as_ptr() as u64]);
    for fd in [r, w, 10] {
        sys(3, &[fd]);
    }
    Ok(())
}

/// Repeated opens of every fabricated file and link lookups neither leak
/// descriptors nor change their answers.
pub fn files_soak() -> Result<(), String> {
    fresh()?;
    let paths = [
        "/etc/passwd",
        "/etc/hosts",
        "/proc/meminfo",
        "/proc/self/stat",
        "/proc/cpuinfo",
    ];
    let names: Vec<Vec<u8>> = paths.iter().map(|p| cpath(p)).collect();
    let mut buf = [0u8; 512];
    for round in 0..1500usize {
        let name = &names[round % names.len()];
        let fd = sys(2, &[name.as_ptr() as u64, 0, 0]);
        check!(
            (fd as i64) >= 3,
            "round {round}: open {:?} = {fd:#x}",
            paths[round % paths.len()]
        );
        let n = sys(0, &[fd, buf.as_mut_ptr() as u64, 512]);
        check!((n as i64) > 0, "round {round}: read {n:#x}");
        check!(sys(3, &[fd]) == 0, "round {round}: close");
        check!(readlink("/proc/self/exe").is_ok(), "round {round}: exe");
    }
    for fd in 3..task::harness::fd_table_len() {
        check!(task::fd_kind(fd) == task::FdKind::Closed, "fd {fd} leaked");
    }
    Ok(())
}

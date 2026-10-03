//! The `/etc` entries backed by real files (docs/tls-plan.md §5.1, §5.2):
//! `/etc/resolv.conf` from `netd`'s file on `/transient`, `/etc/hosts` and the
//! CA bundle under `/etc/ssl/certs` from `/system/etc`. They are read-only,
//! listed only while their file exists, follow every rewrite, and are served
//! only from files a trusted writer owns and nobody else can write.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{AttrRequest, FsError, Id, MountFlags, Vfs};
use alloc::sync::Arc;

const EROFS: u64 = neg(30);
const ENOTDIR: u64 = neg(20);
const O_DIRECTORY: u64 = 0o200000;
const NETD: Id = Id::new(netpolicy::NETD_UID, netpolicy::NETD_UID);
const STRANGER: Id = Id::new(1000, 1000);

fn fs_error(what: &str) -> impl Fn(FsError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// `dir` of the transient file (`/transient/net`).
fn resolv_dir() -> &'static str {
    fhs::state::RESOLV_CONF.rsplit_once('/').unwrap().0
}

/// Create every missing directory of `path`, mode 0755; the last one is
/// owned by `id`.
fn mkdirs(id: Id, path: &str) -> Result<(), String> {
    let mut dir = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        dir.push('/');
        dir.push_str(part);
        match crate::fs::vfs_mkdir(Id::ROOT, &dir, 0o755) {
            Ok(_) | Err(FsError::Exists) => {}
            Err(error) => return Err(format!("mkdir {dir}: {}", error.message())),
        }
    }
    chown(path, id)
}

/// Replace `path` on the native table with `bytes`, mode 0644, owned by `id`.
fn put(id: Id, path: &str, bytes: &[u8]) -> Result<(), String> {
    let _ = crate::fs::vfs_unlink(Id::ROOT, path);
    crate::fs::vfs_create(Id::ROOT, path, 0o644).map_err(fs_error(path))?;
    crate::fs::vfs_write(Id::ROOT, path, 0, bytes).map_err(fs_error(path))?;
    chown(path, id)
}

fn chown(path: &str, id: Id) -> Result<(), String> {
    let owner = AttrRequest::Owner {
        uid: Some(id.uid),
        gid: Some(id.gid),
    };
    crate::fs::vfs_setattr(Id::ROOT, path, owner)
        .map(|_| ())
        .map_err(fs_error(path))
}

fn chmod(path: &str, mode: u16) -> Result<(), String> {
    crate::fs::vfs_setattr(Id::ROOT, path, AttrRequest::Mode(mode))
        .map(|_| ())
        .map_err(fs_error(path))
}

/// Run `body` over a native table with a root volume holding `/system/etc`
/// (and the bundle's directory) and a sticky world-writable `/transient`, as
/// a real boot has; the previous table is put back afterwards.
fn with_native(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    fresh()?;
    let mut vfs = Vfs::new();
    // Room for the oversized-bundle case (past the 4 MiB cap).
    let root = RamFs::with_limits(8 << 20, 256);
    vfs.mount(fhs::mount::ROOT, Arc::new(root), MountFlags::default())
        .map_err(fs_error("mount /"))?;
    vfs.mount(
        fhs::mount::TRANSIENT,
        Arc::new(RamFs::scratch()),
        MountFlags::default(),
    )
    .map_err(fs_error("mount /transient"))?;
    let previous = crate::fs::install_native_for_test(vfs);
    let bundle_dir = fhs::etc::CA_BUNDLE.rsplit_once('/').unwrap().0;
    let result = mkdirs(Id::ROOT, bundle_dir).and_then(|()| body());
    crate::fs::restore_native_for_test(previous);
    result
}

/// `netd`'s directory and file, as `netd` makes them.
fn write_resolv(text: &str) -> Result<(), String> {
    mkdirs(NETD, resolv_dir())?;
    put(NETD, fhs::state::RESOLV_CONF, text.as_bytes())
}

/// Open, read whole and close `path` through the syscall layer.
fn read_via_syscalls(path: &str) -> Result<Vec<u8>, u64> {
    let name = cpath(path);
    let fd = sys(2, &[name.as_ptr() as u64, 0, 0]);
    if (fd as i64) < 0 {
        return Err(fd);
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = sys(0, &[fd, buf.as_mut_ptr() as u64, buf.len() as u64]);
        if (n as i64) <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    sys(3, &[fd]);
    Ok(out)
}

/// `stat(2)`'s return and `st_size`.
fn stat_size(path: &str) -> (u64, u64) {
    let name = cpath(path);
    let mut st = [0u8; 144];
    let ret = sys(4, &[name.as_ptr() as u64, st.as_mut_ptr() as u64]);
    (ret, u64::from_le_bytes(st[48..56].try_into().unwrap()))
}

/// The names `getdents64` lists in `dir`.
fn listing(dir: &str) -> Result<Vec<String>, String> {
    let name = cpath(dir);
    let fd = sys(2, &[name.as_ptr() as u64, O_DIRECTORY, 0]);
    check!((fd as i64) >= 3, "open {dir}: {fd:#x}");
    let mut buf = [0u8; 2048];
    let n = sys(217, &[fd, buf.as_mut_ptr() as u64, buf.len() as u64]);
    sys(3, &[fd]);
    check!((n as i64) > 0, "getdents {dir}: {n:#x}");
    let mut names = Vec::new();
    let mut at = 0;
    while at + 19 <= n as usize {
        let reclen = u16::from_le_bytes([buf[at + 16], buf[at + 17]]) as usize;
        let name = &buf[at + 19..at + reclen];
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        names.push(String::from_utf8_lossy(&name[..end]).into_owned());
        at += reclen;
    }
    Ok(names)
}

/// The three files are served from their backing files, read-only, and
/// listed in `/etc`, `/etc/ssl` and `/etc/ssl/certs`.
pub fn etc_backed_files() -> Result<(), String> {
    with_native(|| {
        let hosts = "127.0.0.1\tlocalhost\n10.0.2.2\ttls.test\n";
        let bundle = "-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n";
        put(Id::ROOT, fhs::etc::HOSTS, hosts.as_bytes())?;
        put(Id::ROOT, fhs::etc::CA_BUNDLE, bundle.as_bytes())?;
        write_resolv("nameserver 10.0.2.3\n")?;
        for (linux, want) in [
            (fhs::etc::LINUX_HOSTS, hosts),
            (fhs::etc::LINUX_CA_BUNDLE, bundle),
            (fhs::etc::LINUX_RESOLV_CONF, "nameserver 10.0.2.3\n"),
        ] {
            check!(fabricated(linux)? == want, "{linux}: {:?}", fabricated(linux));
            let read = read_via_syscalls(linux).map_err(|e| format!("open {linux}: {e:#x}"))?;
            check!(read == want.as_bytes(), "{linux} read {read:?}");
            check!(stat_size(linux) == (0, want.len() as u64), "stat {linux}");
            let name = cpath(linux);
            check!(
                sys(2, &[name.as_ptr() as u64, 1, 0]) == EROFS,
                "{linux} opened for writing"
            );
            check!(
                sys(2, &[name.as_ptr() as u64, O_DIRECTORY, 0]) == ENOTDIR,
                "{linux} opened as a directory"
            );
        }
        let etc = listing(fhs::etc::LINUX_ETC)?;
        for name in ["resolv.conf", "hosts", "ssl", "passwd"] {
            check!(etc.iter().any(|n| n == name), "/etc lacks {name}: {etc:?}");
        }
        check!(
            etc.iter().filter(|n| *n == "hosts").count() == 1,
            "hosts listed twice"
        );
        let ssl = listing(fhs::etc::LINUX_SSL)?;
        check!(ssl.iter().any(|n| n == "certs"), "/etc/ssl: {ssl:?}");
        let certs = listing(fhs::etc::LINUX_SSL_CERTS)?;
        check!(
            certs.iter().any(|n| n == "ca-certificates.crt"),
            "/etc/ssl/certs: {certs:?}"
        );
        // The directories are read-only too.
        let inside = cpath("/etc/ssl/certs/mine.pem");
        check!(
            (sys(2, &[inside.as_ptr() as u64, 0o101, 0o644]) as i64) < 0,
            "created a file in /etc/ssl/certs"
        );
        Ok(())
    })
}

/// Before `netd` writes it there is no `/etc/resolv.conf` (and no listing
/// entry); without a hosts file `localhost` still resolves; without a bundle
/// there is none.
pub fn etc_backed_files_missing() -> Result<(), String> {
    with_native(|| {
        for linux in [fhs::etc::LINUX_RESOLV_CONF, fhs::etc::LINUX_CA_BUNDLE] {
            check!(fabricated(linux).is_err(), "{linux} without its file");
            check!(read_via_syscalls(linux) == Err(ENOENT), "{linux} opened");
            check!(stat_size(linux).0 == ENOENT, "{linux} stat");
        }
        let hosts = fabricated(fhs::etc::LINUX_HOSTS)?;
        check!(
            hosts.contains("127.0.0.1\tlocalhost") && hosts.contains("::1\tlocalhost"),
            "default hosts {hosts:?}"
        );
        let etc = listing(fhs::etc::LINUX_ETC)?;
        check!(!etc.iter().any(|n| n == "resolv.conf"), "listed: {etc:?}");
        check!(etc.iter().any(|n| n == "hosts"), "hosts not listed");
        let certs = listing(fhs::etc::LINUX_SSL_CERTS)?;
        check!(
            !certs.iter().any(|n| n == "ca-certificates.crt"),
            "an absent bundle was listed"
        );
        Ok(())
    })
}

/// A file or directory another user owns or can write is never served:
/// `/transient` is world-writable, so anyone could plant a `resolv.conf`.
pub fn etc_backed_files_untrusted() -> Result<(), String> {
    with_native(|| {
        let resolv = fhs::etc::LINUX_RESOLV_CONF;
        // A stranger got to `/transient/net` first.
        mkdirs(STRANGER, resolv_dir())?;
        put(STRANGER, fhs::state::RESOLV_CONF, b"nameserver 6.6.6.6\n")?;
        check!(fabricated(resolv).is_err(), "a stranger's file was served");
        // `netd`'s file in a stranger's directory still is not.
        put(NETD, fhs::state::RESOLV_CONF, b"nameserver 6.6.6.6\n")?;
        check!(fabricated(resolv).is_err(), "a stranger's directory was trusted");
        crate::fs::vfs_unlink(Id::ROOT, fhs::state::RESOLV_CONF).map_err(fs_error("rm"))?;
        crate::fs::vfs_rmdir(Id::ROOT, resolv_dir()).map_err(fs_error("rmdir"))?;
        write_resolv("nameserver 10.0.2.3\n")?;
        check!(fabricated(resolv).is_ok(), "netd's own file was refused");
        for (path, mode) in [(fhs::state::RESOLV_CONF, 0o666), (resolv_dir(), 0o777)] {
            chmod(path, mode)?;
            check!(fabricated(resolv).is_err(), "served with {path} at {mode:o}");
            chmod(path, if mode == 0o666 { 0o644 } else { 0o755 })?;
        }
        check!(fabricated(resolv).is_ok(), "restored modes refused");
        // Root-only files: `_netd` may not supply the CA bundle or hosts.
        put(NETD, fhs::etc::CA_BUNDLE, b"-----BEGIN CERTIFICATE-----\n")?;
        check!(
            fabricated(fhs::etc::LINUX_CA_BUNDLE).is_err(),
            "a non-root bundle was served"
        );
        put(STRANGER, fhs::etc::HOSTS, b"6.6.6.6\tlocalhost\n")?;
        check!(
            !fabricated(fhs::etc::LINUX_HOSTS)?.contains("6.6.6.6"),
            "a stranger's hosts file was served"
        );
        // Too large to copy into every opener.
        let big = alloc::vec![b'#'; (4 << 20) + 1];
        put(Id::ROOT, fhs::etc::CA_BUNDLE, &big)?;
        check!(
            fabricated(fhs::etc::LINUX_CA_BUNDLE).is_err(),
            "an oversized bundle was served"
        );
        Ok(())
    })
}

/// Soak: `netd` rewrites and removes its file while readers open, stat and
/// list `/etc`; every read sees a whole current version and nothing leaks.
pub fn etc_backed_files_soak() -> Result<(), String> {
    with_native(|| {
        put(Id::ROOT, fhs::etc::HOSTS, b"127.0.0.1\tlocalhost\n")?;
        let warm = fabricated(fhs::etc::LINUX_HOSTS)?;
        let frames_before = crate::mem::frame_stats().live();
        for round in 0..600u32 {
            let text = format!("nameserver 10.0.{}.{}\n", round % 200, round % 7 + 1);
            if round % 5 == 4 {
                let _ = crate::fs::vfs_unlink(NETD, fhs::state::RESOLV_CONF);
                check!(
                    read_via_syscalls(fhs::etc::LINUX_RESOLV_CONF) == Err(ENOENT),
                    "round {round}: removed file still opened"
                );
                continue;
            }
            write_resolv(&text)?;
            let read = read_via_syscalls(fhs::etc::LINUX_RESOLV_CONF)
                .map_err(|e| format!("round {round}: open {e:#x}"))?;
            check!(read == text.as_bytes(), "round {round}: read {read:?}");
            check!(
                stat_size(fhs::etc::LINUX_RESOLV_CONF) == (0, text.len() as u64),
                "round {round}: stat"
            );
            check!(
                fabricated(fhs::etc::LINUX_HOSTS)? == warm,
                "round {round}: hosts changed"
            );
            if round % 50 == 0 {
                let etc = listing(fhs::etc::LINUX_ETC)?;
                check!(etc.iter().any(|n| n == "resolv.conf"), "round {round}");
            }
        }
        for fd in 3..task::harness::fd_table_len() {
            check!(task::fd_kind(fd) == task::FdKind::Closed, "fd {fd} leaked");
        }
        let frames_after = crate::mem::frame_stats().live();
        check!(
            frames_after <= frames_before + 64,
            "frames grew {frames_before} -> {frames_after}"
        );
        Ok(())
    })
}

/// With `fetch` installed as `/system/bin/{fetch,curl,wget}`, every spelling
/// a shell or `execvp` uses runs it, ahead of BusyBox's `wget` applet (which
/// does not verify certificates); without it, `wget` stays BusyBox's.
pub fn tls_client_names() -> Result<(), String> {
    fresh()?;
    let root = Id::ROOT;
    let busybox: &[u8] = b"busybox-bytes";
    let client: &[u8] = b"fetch-client";
    let install = |files: &[(&str, &[u8])]| -> Result<(), String> {
        crate::fs::install_abi_ramfs_for_test();
        for dir in [fhs::SYSTEM, fhs::SYSTEM_BIN] {
            crate::fs::abi_mkdir(root, dir, 0o755).map_err(fs_error(dir))?;
        }
        for (path, bytes) in files {
            crate::fs::abi_create(root, path, 0o755).map_err(fs_error(path))?;
            crate::fs::abi_write(root, path, 0, bytes).map_err(fs_error(path))?;
        }
        Ok(())
    };
    let spellings = |name: &str| {
        alloc::vec![
            String::from(name),
            format!("/bin/{name}"),
            format!("/usr/bin/{name}"),
            format!("/usr/local/bin/{name}"),
            format!("/sbin/{name}"),
            format!("/opt/x/bin/{name}"),
        ]
    };
    install(&[(fhs::bin::BUSYBOX, busybox)])?;
    for path in spellings("wget") {
        check!(
            process::linux::load_executable(&path).as_deref() == Some(busybox),
            "without the client, {path} is not BusyBox's applet"
        );
    }
    install(&[
        (fhs::bin::BUSYBOX, busybox),
        (fhs::bin::FETCH, client),
        (fhs::bin::CURL, client),
        (fhs::bin::WGET, client),
    ])?;
    for name in ["fetch", "curl", "wget"] {
        for path in spellings(name) {
            check!(
                process::linux::load_executable(&path).as_deref() == Some(client),
                "{path} did not run the HTTPS client"
            );
            // `stat` (what ash checks before `execve`) agrees with what runs.
            if path.starts_with('/') && !path.starts_with("/opt") {
                check!(
                    stat_size(&path) == (0, client.len() as u64),
                    "stat {path}: {:?}",
                    stat_size(&path)
                );
            }
        }
    }
    check!(
        process::linux::load_executable("ls").as_deref() == Some(busybox),
        "other applets moved"
    );
    check!(
        process::linux::load_executable("WGET").as_deref() == Some(busybox),
        "names are case-sensitive"
    );
    Ok(())
}

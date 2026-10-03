//! The fabricated `/etc` files a Linux program expects to find: the account
//! and group databases (`getpwnam`, `id`, `ls -l`), the resolver's
//! configuration and host table (musl's `getaddrinfo`), and the OS
//! identification files.
//!
//! LazyOS keeps its configuration elsewhere (`/system/etc/passwd`, in its own
//! `name:uid:gid:secret:home:shell` format, readable by root only), so like
//! [`super::procfs`] these are answered by name, rebuilt on every open and
//! read-only. Only the public columns of the account database are copied:
//! the password column is always `x`.
//!
//! `/etc/resolv.conf` names `10.0.2.3`, the DNS forwarder of QEMU's user-mode
//! network that `netd`'s DHCP lease points at; the kernel does not see the
//! lease itself, so this is the default every supported setup uses.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, Id, Meta};

/// The resolver the default (QEMU user-mode) network provides.
const NAMESERVER: &str = "10.0.2.3";

/// Inode numbers of the fabricated files, clear of `/proc`'s.
const BASE_INO: u64 = 40;

const FILES: &[&str] = &[
    "/etc/passwd",
    "/etc/group",
    "/etc/hosts",
    "/etc/resolv.conf",
    "/etc/hostname",
    "/etc/os-release",
    "/etc/shells",
    "/etc/nsswitch.conf",
];

/// One account of `/system/etc/passwd`, public columns only.
struct Account<'a> {
    name: &'a str,
    uid: &'a str,
    gid: &'a str,
    home: &'a str,
    shell: &'a str,
}

/// Parse LazyOS's account file; malformed lines are skipped.
fn accounts(text: &str) -> Vec<Account<'_>> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [name, uid, gid, _secret, home, shell] = fields[..] else {
                return None;
            };
            let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            (numeric(uid) && numeric(gid) && !name.is_empty()).then_some(Account {
                name,
                uid,
                gid,
                home,
                shell,
            })
        })
        .collect()
}

/// A login shell as a Linux path: LazyOS writes the bare applet name.
fn shell_path(shell: &str) -> String {
    match shell {
        "" | "sh" => String::from("/bin/sh"),
        other if other.starts_with('/') => String::from(other),
        other => format!("/bin/{other}"),
    }
}

/// `/etc/passwd`: `name:x:uid:gid:name:home:shell` per account.
pub(super) fn render_passwd(source: &str) -> String {
    let mut out = String::new();
    for account in accounts(source) {
        out.push_str(&format!(
            "{name}:x:{uid}:{gid}:{name}:{home}:{shell}\n",
            name = account.name,
            uid = account.uid,
            gid = account.gid,
            home = account.home,
            shell = shell_path(account.shell),
        ));
    }
    out
}

/// `/etc/group`: one group per distinct primary gid, named after its first
/// account, with that account as a member.
pub(super) fn render_group(source: &str) -> String {
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for account in accounts(source) {
        if seen.contains(&account.gid) {
            continue;
        }
        seen.push(account.gid);
        out.push_str(&format!(
            "{}:x:{}:{}\n",
            account.name, account.gid, account.name
        ));
    }
    out
}

/// The account file, read with the kernel's identity: only the public columns
/// leave this module.
fn account_source() -> String {
    crate::fs::abi_read(Id::ROOT, fhs::etc::PASSWD)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|text| !accounts(text).is_empty())
        // An image without accounts (the CLI profile) still has the user
        // every task starts as.
        .unwrap_or_else(|| String::from(DEFAULT_ACCOUNTS))
}

/// The accounts of an image that ships no account file: root.
const DEFAULT_ACCOUNTS: &str = "root:0:0::/root:sh\n";

/// The bytes of the fabricated `/etc` file at `path`, if it is one.
pub(super) fn contents(path: &str) -> Option<Vec<u8>> {
    let text = match path {
        "/etc/passwd" => render_passwd(&account_source()),
        "/etc/group" => render_group(&account_source()),
        "/etc/hosts" => String::from("127.0.0.1\tlocalhost lazyos\n::1\tlocalhost\n"),
        "/etc/resolv.conf" => format!("nameserver {NAMESERVER}\n"),
        "/etc/hostname" => String::from("lazyos\n"),
        "/etc/os-release" => {
            String::from("NAME=LazyOS\nID=lazyos\nPRETTY_NAME=\"LazyOS\"\nVERSION_ID=0.1\n")
        }
        "/etc/shells" => String::from("/bin/sh\n/bin/ash\n/bin/dash\n"),
        // Read by glibc-style resolvers; musl ignores it, harmless either way.
        "/etc/nsswitch.conf" => String::from("passwd: files\ngroup: files\nhosts: files dns\n"),
        _ => return None,
    };
    Some(text.into_bytes())
}

/// Metadata for a fabricated `/etc` file: world-readable, as long as its
/// current contents.
pub(super) fn meta(path: &str) -> Option<Meta> {
    let index = FILES.iter().position(|file| *file == path)?;
    let size = contents(path)?.len() as u64;
    Some(Meta {
        ino: BASE_INO + index as u64,
        mode: vfs::S_IFREG | 0o444,
        uid: 0,
        gid: 0,
        size,
        kind: FileKind::File,
        times: vfs::Times::default(),
    })
}

/// The names `/etc` lists.
pub(super) fn names() -> impl Iterator<Item = &'static str> {
    FILES.iter().map(|path| path.trim_start_matches("/etc/"))
}

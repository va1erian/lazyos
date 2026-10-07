//! System configuration files in [`SYSTEM_ETC`](crate::SYSTEM_ETC).

/// The accounts' public columns (`name:uid:gid:x:home:shell`), a view of
/// the account database ([`ACCOUNTS_DB`](crate::state::ACCOUNTS_DB),
/// docs/accounts-plan.md U1) for Linux programs (`/etc/passwd`). 0644, owned
/// by `_accounts`. Written by the image build and `accountsd`.
pub const PASSWD: &str = "/system/etc/passwd";

/// The groups (`name:gid:member,member`), the other view of the account
/// database; the kernel adds it to the Linux `/etc/group`. 0644, owned by
/// `_accounts`. Written by the image build and `accountsd`.
pub const GROUP: &str = "/system/etc/group";

/// The password verifiers of images before U1 (issue #447). The account
/// database holds them now; an update removes this file.
pub const SHADOW: &str = "/system/etc/shadow";

/// The skeleton a new account's home is copied from (docs/accounts-plan.md
/// U1). Written by the image build.
pub const SKEL: &str = "/system/etc/skel";

/// The trust anchors for TLS clients: one PEM bundle generated at build time
/// from a pinned Mozilla root list (docs/tls-plan.md §5.2). Linux programs see
/// it at [`LINUX_CA_BUNDLE`]. Written by the image build.
pub const CA_BUNDLE: &str = "/system/etc/ssl/certs/ca-certificates.crt";

/// Host names known without DNS (`127.0.0.1 localhost`), served to Linux
/// programs as [`LINUX_HOSTS`]. Written by the image build.
pub const HOSTS: &str = "/system/etc/hosts";

/// Present only in an image built with `LAZYOS_UI_PROBE=1` (issue #538): the
/// shell and the LazyRAD player then print the screen rectangles of their
/// named widgets as `UI:` serial lines (the compositor's window lines are
/// compiled in by the same switch), so session scripts click by name
/// (`tools/screenshot/README.md`). Its content is irrelevant; a normal image
/// has no such file and prints nothing.
pub const UI_PROBE: &str = "/system/etc/ui-probe";

/// Where Linux programs look for [`CA_BUNDLE`] by convention
/// (`rustls-native-certs`, OpenSSL, `SSL_CERT_FILE` defaults). Synthesised by
/// the Linux personality's `/etc`.
pub const LINUX_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// musl's resolver configuration, served from
/// [`RESOLV_CONF`](crate::state::RESOLV_CONF). Synthesised by the Linux
/// personality's `/etc`.
pub const LINUX_RESOLV_CONF: &str = "/etc/resolv.conf";

/// The hosts file musl reads before DNS, served from [`HOSTS`].
pub const LINUX_HOSTS: &str = "/etc/hosts";

/// The Linux personality's `/etc`: a synthetic directory listing the
/// fabricated account files and the `LINUX_*` entries below.
pub const LINUX_ETC: &str = "/etc";

/// The directory holding [`LINUX_SSL_CERTS`] (OpenSSL's `OPENSSLDIR`
/// convention). Synthesised by the Linux personality's `/etc`.
pub const LINUX_SSL: &str = "/etc/ssl";

/// The directory holding [`LINUX_CA_BUNDLE`]. Synthesised by the Linux
/// personality's `/etc`.
pub const LINUX_SSL_CERTS: &str = "/etc/ssl/certs";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lives_in_system_etc() {
        assert!(PASSWD.starts_with(crate::SYSTEM_ETC));
        assert!(SHADOW.starts_with(crate::SYSTEM_ETC));
        assert!(GROUP.starts_with(crate::SYSTEM_ETC));
        assert!(SKEL.starts_with(crate::SYSTEM_ETC));
        assert!(CA_BUNDLE.starts_with(crate::SYSTEM_ETC));
        assert!(HOSTS.starts_with(crate::SYSTEM_ETC));
        assert!(UI_PROBE.starts_with(crate::SYSTEM_ETC));
    }

    #[test]
    fn linux_names_nest_and_match_their_files() {
        assert!(LINUX_SSL_CERTS.starts_with(LINUX_SSL));
        let parent = LINUX_CA_BUNDLE.rsplit_once('/').map(|(dir, _)| dir);
        assert_eq!(parent, Some(LINUX_SSL_CERTS));
        // Each Linux name is its system file's path below `/etc`.
        for (system, linux) in [(CA_BUNDLE, LINUX_CA_BUNDLE), (HOSTS, LINUX_HOSTS)] {
            let below_etc = linux.strip_prefix("/etc").unwrap();
            assert!(system.ends_with(below_etc), "{system} vs {linux}");
        }
    }
}

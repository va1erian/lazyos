//! System configuration files in [`SYSTEM_ETC`](crate::SYSTEM_ETC).

/// The account database (`name:uid:gid:secret:home:shell`), read by
/// `accountsd`. Written by the image build. Target (F4): the accounts become
/// `admin` and `user`, and `accountsd` fails closed without it.
pub const PASSWD: &str = "/system/etc/passwd";

/// The trust anchors for TLS clients: one PEM bundle generated at build time
/// from a pinned Mozilla root list (docs/tls-plan.md §5.2). Linux programs see
/// it at [`LINUX_CA_BUNDLE`]. Written by the image build.
pub const CA_BUNDLE: &str = "/system/etc/ssl/certs/ca-certificates.crt";

/// Host names known without DNS (`127.0.0.1 localhost`), served to Linux
/// programs as [`LINUX_HOSTS`]. Written by the image build.
pub const HOSTS: &str = "/system/etc/hosts";

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
        assert!(CA_BUNDLE.starts_with(crate::SYSTEM_ETC));
        assert!(HOSTS.starts_with(crate::SYSTEM_ETC));
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

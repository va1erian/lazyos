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

#[cfg(test)]
mod tests {
    #[test]
    fn lives_in_system_etc() {
        assert!(super::PASSWD.starts_with(crate::SYSTEM_ETC));
        assert!(super::CA_BUNDLE.starts_with(crate::SYSTEM_ETC));
        assert!(super::HOSTS.starts_with(crate::SYSTEM_ETC));
    }
}

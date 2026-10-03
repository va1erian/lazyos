//! System configuration files in [`SYSTEM_ETC`](crate::SYSTEM_ETC).

/// The account database (`name:uid:gid:secret:home:shell`), read by
/// `accountsd`. Written by the image build. Target (F4): the accounts become
/// `admin` and `user`, and `accountsd` fails closed without it.
pub const PASSWD: &str = "/system/etc/passwd";

#[cfg(test)]
mod tests {
    #[test]
    fn lives_in_system_etc() {
        assert!(super::PASSWD.starts_with(crate::SYSTEM_ETC));
    }
}

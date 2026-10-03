//! The resolver configuration `netd` writes for Linux programs
//! (`fhs::state::RESOLV_CONF`, read by musl's `getaddrinfo` as
//! `/etc/resolv.conf`; docs/tls-plan.md §5.1).
//!
//! Only the resolvers the stack holds are written, and those were validated
//! when the lease or the static configuration was applied
//! ([`crate::config::is_usable_unicast`], at most [`MAX_DNS`]); they are
//! filtered again here so a future caller cannot slip anything else into the
//! file. The text is nothing but `nameserver a.b.c.d` lines, so no option a
//! DHCP server could choose (`search`, `options`) ever reaches a resolver.

use alloc::format;
use alloc::string::String;

use crate::config::is_usable_unicast;
use crate::stack::MAX_DNS;

/// The file's first line, so a reader knows where it came from.
pub const HEADER: &str = "# Written by netd from its DHCP lease or static configuration.\n";

/// The `resolv.conf` text for `dns`, or `None` when there is no usable
/// resolver (the file is then removed: musl falls back to `127.0.0.1`, which
/// answers nothing, rather than to a stale server).
pub fn render(dns: &[[u8; 4]]) -> Option<String> {
    let mut out = String::from(HEADER);
    let mut count = 0;
    for addr in dns.iter().filter(|addr| is_usable_unicast(**addr)) {
        if count == MAX_DNS {
            break;
        }
        out.push_str(&format!(
            "nameserver {}.{}.{}.{}\n",
            addr[0], addr[1], addr[2], addr[3]
        ));
        count += 1;
    }
    (count > 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_user_network() {
        assert_eq!(
            render(&[[10, 0, 2, 3]]).as_deref(),
            Some("# Written by netd from its DHCP lease or static configuration.\nnameserver 10.0.2.3\n")
        );
    }

    #[test]
    fn nothing_usable_means_no_file() {
        assert_eq!(render(&[]), None);
        assert_eq!(
            render(&[[0, 0, 0, 0], [127, 0, 0, 1], [224, 0, 0, 1]]),
            None
        );
    }

    #[test]
    fn unusable_entries_are_dropped_and_the_count_capped() {
        let text = render(&[
            [127, 0, 0, 1],
            [1, 1, 1, 1],
            [8, 8, 8, 8],
            [255, 255, 255, 255],
            [9, 9, 9, 9],
            [8, 8, 4, 4],
        ])
        .unwrap();
        let servers: alloc::vec::Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("nameserver "))
            .collect();
        assert_eq!(servers, ["1.1.1.1", "8.8.8.8", "9.9.9.9"]);
        assert!(text
            .lines()
            .all(|line| line.starts_with('#') || line.starts_with("nameserver ")));
    }
}

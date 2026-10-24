//! The key that places a tab's views in webd.

use crate::address;

/// The key that places a tab's views in webd (`ViewConfig.process_key`, docs/lazyweb-plan.md 2.3):
/// the host of `url`, lower case and without a port, or empty for anything that is not a web page
/// (the browser's own pages share the default process). At most 64 printable ASCII bytes, as webd
/// takes; a longer or odder host is left to the default.
pub fn process_key(url: &str) -> String {
    if !address::is_network(url) {
        return String::new();
    }
    let rest = url.split_once("://").map_or("", |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // A port follows the host, except inside the brackets of an IPv6 address.
    let host = match host.strip_prefix('[') {
        Some(inner) => inner.split_once(']').map_or("", |(ip, _)| ip),
        None => host.split(':').next().unwrap_or(""),
    };
    let host = host.to_ascii_lowercase();
    let fits = !host.is_empty() && host.len() <= 64 && host.bytes().all(|b| b.is_ascii_graphic());
    if fits {
        host
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::process_key;

    #[test]
    fn a_tabs_process_is_its_hosts() {
        assert_eq!(process_key("https://Example.COM/a?b#c"), "example.com");
        assert_eq!(
            process_key("http://user:pw@news.example:8080/x"),
            "news.example"
        );
        assert_eq!(process_key("https://[::1]:8443/"), "::1");
        assert_eq!(process_key("https://www.example.com"), "www.example.com");
    }

    #[test]
    fn what_is_not_a_web_page_or_a_plain_host_has_no_key() {
        assert_eq!(process_key("data:text/html,hi"), "");
        assert_eq!(process_key("about:blank"), "");
        assert_eq!(process_key(""), "");
        assert_eq!(process_key("https:///nohost"), "");
        let long = format!("https://{}.example/", "a".repeat(80));
        assert_eq!(process_key(&long), "");
        assert_eq!(process_key("https://sp ace.example/"), "");
    }
}

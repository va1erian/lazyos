//! Topic filter grammar: parsing and validating subscription filters and
//! literal publish topics against the same byte set and depth limits as the
//! kernel ACL gate (`kernel/src/ipc/topics.rs`).

use alloc::string::String;
use alloc::vec::Vec;

/// Longest topic/filter name, mirroring the kernel ACL gate.
const MAX_NAME_BYTES: usize = 128;
/// Deepest topic/filter, mirroring the kernel ACL gate.
const MAX_SEGMENTS: usize = 8;

/// A parsed subscription filter: literal segments plus `+` and `#` wildcards.
pub(super) struct Filter {
    pub(super) segments: Vec<String>,
}

impl Filter {
    /// Parse and validate; `None` for anything the ACL gate would refuse, so
    /// the broker and the kernel agree on what a valid filter is.
    pub(super) fn parse(text: &str) -> Option<Filter> {
        if text.is_empty() || text.len() > MAX_NAME_BYTES {
            return None;
        }
        let segments: Vec<String> = text.split('/').map(String::from).collect();
        if segments.is_empty() || segments.len() > MAX_SEGMENTS {
            return None;
        }
        for (index, segment) in segments.iter().enumerate() {
            if !valid_segment(segment) {
                return None;
            }
            // `#` may only stand alone and only last; anywhere else it would
            // silently shadow a literal name.
            if segment.contains('#') && (segment != "#" || index + 1 != segments.len()) {
                return None;
            }
        }
        Some(Filter { segments })
    }

    /// Whether this filter matches a (literal) topic name. `+` consumes one
    /// segment; a trailing `#` consumes zero or more.
    pub(super) fn matches(&self, topic: &str) -> bool {
        let topic_segments: Vec<&str> = topic.split('/').collect();
        let mut topic_index = 0;
        for segment in &self.segments {
            if segment == "#" {
                return true;
            }
            if topic_index >= topic_segments.len() {
                return false;
            }
            if segment != "+" && segment != topic_segments[topic_index] {
                return false;
            }
            topic_index += 1;
        }
        topic_index == topic_segments.len()
    }
}

/// Whether `segment` is a legal literal/wildcard segment (same rules as the
/// kernel ACL gate in `kernel/src/ipc/topics.rs`, dot-only segments included).
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_NAME_BYTES
        && !segment.bytes().all(|byte| byte == b'.')
        && segment.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
        })
}

/// A publish topic must be literal: no wildcards (the kernel refuses them in
/// publish mode too).
pub(super) fn valid_topic(topic: &str) -> bool {
    if topic.is_empty() || topic.len() > MAX_NAME_BYTES {
        return false;
    }
    let mut count = 0;
    for segment in topic.split('/') {
        if !valid_segment(segment) || segment.contains('+') || segment.contains('#') {
            return false;
        }
        count += 1;
        if count > MAX_SEGMENTS {
            return false;
        }
    }
    count > 0
}

/// Whether `topic` falls under the platform's reserved `system/` root.
pub(super) fn is_system_topic(topic: &str) -> bool {
    topic == "system" || topic.starts_with("system/")
}

/// Whether `topic` is one NIC's link topic, `system/net/<nic>/link`: all
/// the NIC driver publishes. The rest of `system/net/` (the stack's retained
/// `addr`) is not the driver's to overwrite.
fn is_nic_link(topic: &str) -> bool {
    topic
        .strip_prefix("system/net/")
        .and_then(|rest| rest.strip_suffix("/link"))
        .is_some_and(|nic| !nic.is_empty() && !nic.contains('/'))
}

/// Whether a task of `uid` may publish `topic` under `system/`. Everything
/// there is root's but what a dedicated system uid owns (issue #497): the
/// device manager its devices, the NIC driver its link.
pub(super) fn may_publish_system(topic: &str, uid: u32) -> bool {
    uid == 0
        || (uid == devmatch::DEVD_UID && topic.starts_with("system/devices/"))
        || (uid == netpolicy::NET_UID && is_nic_link(topic))
}

//! Topic filter grammar: parsing and validating subscription filters and
//! literal publish topics against the same byte set and depth limits as the
//! kernel ACL gate (`kernel/src/ipc/topics.rs`).

pub(super) use topicsbroker::{is_system_topic, valid_topic};

/// The card a NIC link topic, `system/net/<nic>/link`, is about: all the NIC
/// driver publishes. The rest of `system/net/` (the stack's retained `addr`)
/// is not the driver's to overwrite.
pub(super) fn nic_link_ifname(topic: &str) -> Option<&str> {
    topic
        .strip_prefix("system/net/")
        .and_then(|rest| rest.strip_suffix("/link"))
        .filter(|nic| !nic.is_empty() && !nic.contains('/'))
}

fn is_nic_link(topic: &str) -> bool {
    nic_link_ifname(topic).is_some()
}

/// Whether the task in slot `sender` holds the registry name of the card
/// `ifname` (`os.lazy.net.nic/<ifname>`). A NIC driver uid may publish the
/// link of its own card only: `eth1`'s driver must not say `eth0` went
/// down, nor a radio driver speak for a wired card. One `List` per link
/// publish, which happens on a link change only.
pub(super) fn holds_nic_name(sender: u64, ifname: &str) -> bool {
    let name = alloc::format!("{}/{ifname}", netpolicy::NIC_NAME_PREFIX);
    user::messenger::registry::list().is_ok_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.name == name && entry.owner_slot == sender)
    })
}

/// Whether a task of `uid` may publish `topic` under `system/`. Everything
/// there is root's but what a dedicated system uid owns (issue #497): the
/// device manager its devices, a NIC driver (wired or a Wi-Fi radio) its
/// link, the stack its address and the network-up event
/// (`netpolicy::is_stack_topic`), the station manager its Wi-Fi state and
/// scan (`netpolicy::is_wifi_topic`), `elevd` its audit trail
/// (docs/accounts-plan.md U2).
pub(super) fn may_publish_system(topic: &str, uid: u32) -> bool {
    uid == 0
        || (uid == devmatch::DEVD_UID && topic.starts_with("system/devices/"))
        || (uid == elevpolicy::ELEVD_UID && topic.starts_with("system/events/elevd/"))
        || (uid == netpolicy::NET_UID && is_nic_link(topic))
        || ((uid == netpolicy::WIFI_UID || uid == netpolicy::WIFISIM_UID) && is_nic_link(topic))
        || (uid == netpolicy::NETD_UID && netpolicy::is_stack_topic(topic))
        || (uid == netpolicy::WLAN_UID && netpolicy::is_wifi_topic(topic))
}

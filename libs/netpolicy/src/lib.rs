//! The access-control rules of the networking stack, as data.
//!
//! Policy is a list of `(actor uid, interface, method, allow)` rules the kernel
//! evaluates first-match, default deny once a policy is installed
//! (`docs/architecture/ipc-security.md`). The device-class rules
//! ([`NET_DRIVER_CLASS_RULES`]) are installed by the kernel at boot
//! (`dev::policy`, issue #481), so `_net` gets the net class and nothing else
//! on every boot. The call rules wait for a Messenger policy loader
//! (`messengerd`, `docs/security-model.md`): the fabric is still in its
//! bootstrap-allow window, so they refuse nothing yet. The kernel test suite
//! loads these tables and checks the decisions they yield
//! (`dev_suite::sys_net_driver_policy_is_exactly_the_class_rules`,
//! `dev_suite::sys_boot_policy_confines_each_driver_to_its_class`).
//!
//! Interfaces and methods are spelled as names; the loader hashes them the way
//! every Messenger id is hashed (`tools/midlc`), and a host test here pins the
//! spelling to the `midlc`-generated ids.

#![no_std]

/// The `_net` system user the NIC driver runs as: only `CAP_DEV_CLAIM`.
pub const NET_UID: u32 = 902;

/// The `_netd` system user the network stack runs as: **no** capabilities.
pub const NETD_UID: u32 = 903;

/// The `_wifi` system user the Wi-Fi chip driver (`wifid`,
/// docs/wifi-rtw88-plan.md) runs as: only `CAP_DEV_CLAIM`, and only the
/// wireless device class ([`WIFI_DRIVER_CLASS_RULES`]).
pub const WIFI_UID: u32 = 911;

/// The `_wlan` system user: the Wi-Fi station manager (`wlanmd`), **no**
/// capabilities. The one identity a radio accepts as its client, and the one
/// `keyd` gives PMKs to (`secretstore::WLAN_UID`, the same value).
pub const WLAN_UID: u32 = 912;

/// The `_wifisim` system user: the simulated Wi-Fi chip the tests run.
pub const WIFISIM_UID: u32 = 913;

/// Matches any actor (the kernel's `ANY_ACTOR`).
pub const ANY_ACTOR: u32 = u32::MAX;

/// Root. It has ambient authority anyway; the NIC rules name it so that the
/// diagnostic tools (`nicctl`, run from a root shell) keep working.
pub const ROOT_UID: u32 = 0;

/// Matches any method of the interface.
pub const ANY_METHOD: &str = "*";

/// The NIC driver's interface (`idl/net.midl`).
pub const NIC_INTERFACE: &str = "os.lazy.net.nic.v1";
/// The stack service's interface (`idl/net.midl`).
pub const STACK_INTERFACE: &str = "os.lazy.net.stack.v1";
/// The socket interface `netd` serves next to it (`idl/net.midl`, stage N3).
pub const SOCKET_INTERFACE: &str = "os.lazy.net.socket.v1";

mod nic;
pub use nic::{
    is_nic_name, may_own_nic, may_register_nic_name, nic_kind_allowed, stack_attaches,
    NIC_KIND_WIRED, NIC_KIND_WIRELESS, NIC_NAME_PREFIX, SIM_AIR_IFNAME, SIM_AIR_MAC,
};
mod wifi;
pub use wifi::{
    is_wifi_hw_name, is_wifi_service_name, is_wifi_topic, is_wlanmd, may_attach_wifi_hw,
    may_register_wifi_hw_name, may_register_wifi_service_name, WIFI_HW_NAME_PREFIX,
    WIFI_SERVICE_NAME,
};

/// One rule. `actor` is a uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleSpec {
    pub actor: u32,
    pub interface: &'static str,
    pub method: &'static str,
    pub allow: bool,
}

const fn allow(actor: u32, interface: &'static str, method: &'static str) -> RuleSpec {
    RuleSpec {
        actor,
        interface,
        method,
        allow: true,
    }
}

const fn deny(actor: u32, interface: &'static str, method: &'static str) -> RuleSpec {
    RuleSpec {
        actor,
        interface,
        method,
        allow: false,
    }
}

/// What `_net` may do to a device: claim a device *of the net class* and take
/// the MMIO/PIO and DMA rights a NIC driver needs (`docs/driver-plan.md`
/// section 3.5). Nothing names another class, so `_net` cannot claim storage,
/// audio or anything else, and nothing grants another uid the net class.
pub const NET_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(NET_UID, "os.kernel.dev.net", "claim"),
    allow(NET_UID, "os.kernel.dev.net", "map"),
    allow(NET_UID, "os.kernel.dev.net", "dma"),
];

/// What `_wifi` may do to a device: the same three rights, on the *wireless*
/// class only (PCI `02/80`, `0D/20` and `0D/21`, the kernel's
/// `dev::class::WIRELESS`), so `wifid` cannot claim the wired card `dbgd`
/// talks through, and `netdrv` cannot claim the radio.
pub const WIFI_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(WIFI_UID, "os.kernel.dev.wireless", "claim"),
    allow(WIFI_UID, "os.kernel.dev.wireless", "map"),
    allow(WIFI_UID, "os.kernel.dev.wireless", "dma"),
];

/// Who may call the NIC driver. `_netd` is the stack and the only real client;
/// anyone may read the card (`Info`, `Stats`); root is named for the diagnostic
/// tools (and cannot displace an attached `_netd`: the driver accepts one
/// client and refuses a second). `_wifisim` is the client of the simulated
/// radio's air card (`air0`, which `netd` never attaches): the ring methods
/// only, and only on that card, which this per-interface table cannot say
/// and every NIC server enforces with [`may_own_nic`]. Everything else,
/// `Kick` and `Notify` included, is refused, which is what stops an
/// application from attaching to the card or feeding the driver frames.
pub const NIC_CLIENT_RULES: &[RuleSpec] = &[
    allow(NETD_UID, NIC_INTERFACE, ANY_METHOD),
    allow(ROOT_UID, NIC_INTERFACE, ANY_METHOD),
    allow(WIFISIM_UID, NIC_INTERFACE, "AttachRing"),
    allow(WIFISIM_UID, NIC_INTERFACE, "DetachRing"),
    allow(WIFISIM_UID, NIC_INTERFACE, "SetRxMode"),
    allow(WIFISIM_UID, NIC_INTERFACE, "Kick"),
    allow(ANY_ACTOR, NIC_INTERFACE, "Info"),
    allow(ANY_ACTOR, NIC_INTERFACE, "Stats"),
    // The driver's own wake-up to the client: `Notify` is sent *by* the
    // driver (`_net`, or a Wi-Fi radio serving `wlan0`, the simulated one
    // included), to the notify endpoint the client handed it, and is judged
    // like any call.
    allow(NET_UID, NIC_INTERFACE, "Notify"),
    allow(WIFI_UID, NIC_INTERFACE, "Notify"),
    allow(WIFISIM_UID, NIC_INTERFACE, "Notify"),
    deny(ANY_ACTOR, NIC_INTERFACE, ANY_METHOD),
];

/// Who may call the stack service: anyone reads it and pings; changing it
/// (`Renew`, `Reattach`) is for `_netd` itself and root.
pub const STACK_CLIENT_RULES: &[RuleSpec] = &[
    allow(ANY_ACTOR, STACK_INTERFACE, "Interfaces"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Addresses"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Routes"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Stats"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Ping"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Resolve"),
    allow(NETD_UID, STACK_INTERFACE, ANY_METHOD),
    allow(ROOT_UID, STACK_INTERFACE, ANY_METHOD),
    deny(ANY_ACTOR, STACK_INTERFACE, ANY_METHOD),
];

/// The socket methods, in the order of the interface. Each is its own rule so
/// that a profile can later grant `Connect` without `Listen` (stage N6 narrows
/// who gets which; until then any caller may use sockets, and `netd` enforces
/// ownership of each socket and its own quotas whatever this table says).
pub const SOCKET_METHODS: &[&str] = &[
    "Open",
    "Bind",
    "Connect",
    "Listen",
    "Accept",
    "Send",
    "Recv",
    "SendTo",
    "RecvFrom",
    "Poll",
    "Shutdown",
    "LocalAddr",
    "PeerAddr",
    "Close",
    "Stats",
];

const fn socket_allow(method: &'static str) -> RuleSpec {
    allow(ANY_ACTOR, SOCKET_INTERFACE, method)
}

/// Who may call the socket service: every method is open to every caller for
/// now, and anything that is not a socket method is refused.
pub const SOCKET_CLIENT_RULES: &[RuleSpec] = &[
    socket_allow("Open"),
    socket_allow("Bind"),
    socket_allow("Connect"),
    socket_allow("Listen"),
    socket_allow("Accept"),
    socket_allow("Send"),
    socket_allow("Recv"),
    socket_allow("SendTo"),
    socket_allow("RecvFrom"),
    socket_allow("Poll"),
    socket_allow("Shutdown"),
    socket_allow("LocalAddr"),
    socket_allow("PeerAddr"),
    socket_allow("Close"),
    socket_allow("Stats"),
    deny(ANY_ACTOR, SOCKET_INTERFACE, ANY_METHOD),
];

/// Whether `topic` is one the stack (`netd`, [`NETD_UID`]) publishes under
/// the reserved `system/` root: an interface's retained address
/// (`system/net/<if>/addr`), the list of interfaces (`system/net/interfaces`)
/// and the network-up event (`system/events/network/up`), all declared in
/// `idl/net.midl`. Nothing
/// else under `system/` is the stack's (a NIC's link is its driver's).
pub fn is_stack_topic(topic: &str) -> bool {
    let addr = topic
        .strip_prefix("system/net/")
        .and_then(|rest| rest.strip_suffix("/addr"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'));
    addr || topic == "system/net/interfaces" || topic == "system/events/network/up"
}

#[cfg(test)]
mod tests;

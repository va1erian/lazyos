//! The identities and arguments `init` starts its dedicated services with:
//! each driver, stack and account service under its own system uid, holding
//! only the capability it needs ([`super::state::manifest_cred`]).
//!
//! Split out of `state.rs` (file-length limit); a pure move.

use user::sys::Cred as SysCred;

/// The `sndd` driver's identity (docs/driver-plan.md D3): a dedicated system
/// uid holding only `CAP_DEV_CLAIM`, so a compromised driver has the device it
/// claimed and nothing else: no `CAP_SETUID`, no path to uid 0.
#[cfg(lazyos_sound)]
pub(super) const SND_CRED: SysCred = SysCred::new(SND_UID, SND_UID, user::dev::CAP_DEV_CLAIM, 0, 0);
/// The `_snd` system user.
#[cfg(lazyos_sound)]
pub(super) const SND_UID: u32 = sndpolicy::SND_UID;

/// The mixer's identity (docs/audio-plan.md): its own system uid and **no
/// capabilities at all**. It maps the rings clients hand it and owns the
/// card's stream; it has no device, no DMA and no authority over anyone.
#[cfg(lazyos_sound)]
pub(super) const AUDIO_CRED: SysCred =
    SysCred::new(sndpolicy::AUDIO_UID, sndpolicy::AUDIO_UID, 0, 0, 0);

/// `usbd`'s arguments. `trace=1` echoes every report and key edge on serial,
/// which would put typed passwords on the console, so only the USB harness's
/// test images (`LAZYOS_USB_TRACE=1`, `tools/usb/run.py`) turn it on.
#[cfg(lazyos_usb)]
pub(super) const USBD_ARGS: &str = if cfg!(lazyos_usb_trace) {
    "trace=1"
} else {
    ""
};

/// The `usbd` driver's identity (docs/usb-hid-plan.md U2): a dedicated system
/// uid holding only `CAP_DEV_CLAIM` (the controller), `CAP_INPUT_SOURCE`
/// (publishing its devices' input) and `CAP_BLOCK_PROVIDER` (serving a USB
/// stick to the kernel, docs/architecture/usb-storage.md). It cannot read the
/// input bus, and the kernel stamps its records with device ids of their own.
#[cfg(lazyos_usb)]
pub(super) const USB_CRED: SysCred = SysCred::new(
    USB_UID,
    USB_UID,
    user::dev::CAP_DEV_CLAIM | user::sys::CAP_INPUT_SOURCE | user::sys::CAP_BLOCK_PROVIDER,
    0,
    0,
);
/// The `_usb` system user (901 `_snd`, 902 `_net`, 903 `_netd`).
#[cfg(lazyos_usb)]
pub(super) const USB_UID: u32 = usbpolicy::USB_UID;

/// The `netdrv` driver's identity (docs/networking-plan.md N1): a dedicated
/// system uid holding only `CAP_DEV_CLAIM`, exactly like `sndd`'s.
#[cfg(lazyos_net)]
pub(super) const NET_CRED: SysCred = SysCred::new(NET_UID, NET_UID, user::dev::CAP_DEV_CLAIM, 0, 0);
/// The `_net` system user.
#[cfg(lazyos_net)]
pub(super) const NET_UID: u32 = netpolicy::NET_UID;

/// The device manager's identity (issue #497): its own system uid and **no
/// capabilities**. It reads the kernel's read-only inventory and asks this
/// supervisor to start drivers; it never claims a device.
#[cfg(lazyos_devd)]
pub(super) const DEVD_CRED: SysCred = SysCred::new(devmatch::DEVD_UID, devmatch::DEVD_UID, 0, 0, 0);

/// The argument string of the `netdrv` row: `demo=1` runs the self-test and the
/// evidence clients; `LAZYOS_NET_ARGS` overrides it at build time (the network
/// harness's `--poll` passes `demo=1 irq=poll`).
#[cfg(lazyos_net)]
pub(super) const NET_ARGS: &str = match option_env!("LAZYOS_NET_ARGS") {
    Some(args) => args,
    // With `netd` present the driver runs only its ARP self-test: the evidence
    // clients attach to the NIC, and `netd` holds the one attachment.
    None if cfg!(lazyos_netd) => "selftest=1",
    None => "demo=1",
};

/// The argument string of the `netd` row: `demo=1` runs the evidence clients
/// the network harness judges (they talk to its host servers);
/// `LAZYOS_NETD_ARGS` overrides it at build time (`run_demo.py --net` passes
/// `demo=0`, so an interactive desktop runs the stack alone).
#[cfg(lazyos_netd)]
pub(super) const NETD_ARGS: &str = match option_env!("LAZYOS_NETD_ARGS") {
    Some(args) => args,
    None => "demo=1",
};

/// The `netd` stack service's identity (docs/networking-plan.md N2): its own
/// system uid and **no capabilities at all**: it holds no device authority, no
/// DMA, nothing it could misuse if a parser bug handed an attacker the process.
#[cfg(lazyos_netd)]
pub(super) const NETD_CRED: SysCred = SysCred::new(NETD_UID, NETD_UID, 0, 0, 0);
/// The `_netd` system user.
#[cfg(lazyos_netd)]
pub(super) const NETD_UID: u32 = netpolicy::NETD_UID;

/// The account database service's identity (docs/accounts-plan.md U1): the
/// `_accounts` system uid, owner of `/accounts`, and **no capability**.
/// Whatever it needs done as root (a home) it asks this supervisor for, and
/// `keyd` takes verifiers from this identity alone.
pub(super) const ACCOUNTS_CRED: SysCred =
    SysCred::new(accountdb::ACCOUNTS_UID, accountdb::ACCOUNTS_UID, 0, 0, 0);

/// The elevation service's identity (docs/accounts-plan.md U2): the `_elev`
/// system uid and **no capability**. The services it calls accept its
/// privileged requests by this identity alone.
pub(super) const ELEVD_CRED: SysCred =
    SysCred::new(accountdb::ELEVD_UID, accountdb::ELEVD_UID, 0, 0, 0);

/// The network mount service's identity (docs/smb-plan.md §3.4): its own
/// system uid holding only `CAP_FS_PROVIDER`, which the `ftpfuse` daemons it
/// starts inherit: they may serve `/mnt/<name>` and nothing more.
#[cfg(lazyos_netd)]
pub(super) const MOUNTD_CRED: SysCred = SysCred::new(
    mounttable::MOUNTD_UID,
    mounttable::MOUNTD_UID,
    user::sys::fuse::CAP_FS_PROVIDER,
    0,
    0,
);

/// Every system uid a service runs as or is trusted by. Services authorize
/// by uid (logind's `Login` takes `_greeter`'s alone, accountsd trusts
/// `_greeter` and `_elev`), so two services sharing one would let either
/// pass for the other: the build fails instead.
const SYSTEM_UIDS: [u32; 10] = [
    sndpolicy::SND_UID,
    netpolicy::NET_UID,
    netpolicy::NETD_UID,
    usbpolicy::USB_UID,
    sndpolicy::AUDIO_UID,
    devmatch::DEVD_UID,
    user::messenger::logind::GREETER_UID,
    accountdb::ACCOUNTS_UID,
    accountdb::ELEVD_UID,
    mounttable::MOUNTD_UID,
];

const _: () = {
    let mut i = 0;
    while i < SYSTEM_UIDS.len() {
        let mut j = i + 1;
        while j < SYSTEM_UIDS.len() {
            assert!(SYSTEM_UIDS[i] != SYSTEM_UIDS[j], "two system services share a uid");
            j += 1;
        }
        i += 1;
    }
};

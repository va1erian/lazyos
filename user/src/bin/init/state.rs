//! `init`'s supervision model: the tuning constants that shape restart/backoff
//! and session-launch policy, the boot manifest, the runtime service rows the
//! supervisor tracks, and the restart policy shared with the app registry.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use user::sys::Cred as SysCred;

// The restart/backoff tuning and the restart policy live in the host-tested
// `svcpolicy` crate (issue #549); re-exported so the supervisor keeps one
// import path.
pub(super) use svcpolicy::{Restart, MAX_RESTARTS};

#[allow(unused_imports)] // which constants exist depends on the image's cfgs
use super::service_creds::*;
/// Capabilities a launched session child receives. Empty today, matching
/// `logind`'s session set: least privilege is the default and the S5.2
/// session grants arrive through the credential gate.
pub(super) const SESSION_CAPS: u32 = 0;
/// `CAP_SETUID` (kernel `ipc::credentials`): a supervisor holding it may
/// launch into any session.
pub(super) const CAP_SETUID: u32 = 1 << 6;
/// Ticks between launch self-test (and autostart) attempts while no task
/// slot is free.
pub(super) const LAUNCH_SELFTEST_RETRY: u64 = 25;
/// Give up on the launch self-test after this many attempts.
pub(super) const LAUNCH_SELFTEST_ATTEMPTS: u64 = 40;
/// Autostart retries per app while the task table is full.
pub(super) const AUTOSTART_ATTEMPTS: u64 = 40;
/// Launched rows one session may hold reserved at once (issue #177). The
/// cap is a fixed number, not derived from the live task table
/// (`kernel/src/task/mod.rs`'s `MAX_TASKS`): it stops one session looping
/// `launch` until the table is full and supervised restarts and new logins
/// starve. The application package system raised both to 256 so a session can
/// hold many installed apps; the boot services and other sessions share the
/// same 256 slots, so the kernel table, not this cap, is what runs out first
/// when several sessions are busy. A row still reserves its slot
/// while `Restarting`: [`spawn_service`] respawns it from the main loop's
/// backoff sweep, not through [`launch`], so a crashed row that stopped
/// counting here could let a session accumulate more rows than the cap once
/// they all came back up. [`running_in_session`] counts every phase that
/// currently holds or will reclaim a slot without another cap check.
pub(super) const LAUNCH_CAP_PER_SESSION: usize = 256;

/// Credentials a manifest row is spawned with; `None` inherits this
/// supervisor's identity, which is what the platform services need.
pub(super) fn manifest_cred(name: &str) -> Option<SysCred> {
    if name == "accountsd" {
        return Some(ACCOUNTS_CRED);
    }
    if name == "elevd" {
        return Some(ELEVD_CRED);
    }
    #[cfg(lazyos_sound)]
    if name == "sndd" {
        return Some(SND_CRED);
    }
    #[cfg(lazyos_sound)]
    if name == "audiod" {
        return Some(AUDIO_CRED);
    }
    #[cfg(lazyos_usb)]
    if name == "usbd" {
        return Some(USB_CRED);
    }
    #[cfg(lazyos_net)]
    if name == "netdrv" {
        return Some(NET_CRED);
    }
    #[cfg(lazyos_netd)]
    if name == "netd" {
        return Some(NETD_CRED);
    }
    #[cfg(lazyos_netd)]
    if name == "mountd" {
        return Some(MOUNTD_CRED);
    }
    #[cfg(lazyos_devd)]
    if name == "devd" {
        return Some(DEVD_CRED);
    }
    let _ = name;
    None
}

/// Whether this is the desktop profile (`LAZYOS_DESKTOP=1`, issue #217): the
/// image is a user-facing session, not an evidence boot. `init` keeps the
/// demo-only programs out of it — the deliberate-crash service, the clipboard
/// demo pair and the `top` launch self-test — so a desktop log shows only the
/// real services and apps.
const DESKTOP: bool = cfg!(lazyos_desktop);

/// Whether this boot runs its self-tests (soak, demo clients, launch checks).
/// They are evidence for headless/CI runs and cost boot time, so optimized
/// release builds leave them out, as does the desktop profile (a desktop boot
/// starts only the real session, never the evidence programs).
pub(super) const BOOT_SELFTESTS: bool = cfg!(debug_assertions);

/// Whether this boot starts the evidence-only *programs* (the `soak=`/`demo=`
/// clients, the `flaky` crash service and the `top` launch self-test). Kept
/// separate from [`BOOT_SELFTESTS`] because the desktop profile still serves
/// the registry and policy self-tests but must not start demo programs.
pub(super) const BOOT_EVIDENCE: bool = BOOT_SELFTESTS && !DESKTOP;

/// One manifest row: the fields the supervisor needs to start and watch a
/// service. The service's retained health topic is derived from its name via
/// the generated `system/health/{name}` helper, so it is not stored here.
pub(super) struct ServiceSpec {
    pub(super) name: &'static str,
    pub(super) path: &'static str,
    pub(super) args: &'static str,
    pub(super) restart: Restart,
    pub(super) deps: &'static [&'static str],
}

/// The boot manifest. `messengerd` is first because it owns the bootstrap
/// registry listener; `keyd`, `logd` and `healthd` depend on it. `accountsd`
/// and `logind` only need the kernel's name registry (which every task can use
/// directly), and `logind` declares its dependency on `accountsd` so the
/// supervisor starts login once accounts are up. `flaky` depends on `healthd`
/// so the crash test also proves dependency gating. `messengerd` is `Once`
/// because the kernel's bootstrap channel can be claimed only once per boot, so
/// restarting it could not re-listen. (`logind`'s console dialog shows in
/// `init`'s window: the kernel routes a child's terminal to its root ancestor,
/// so the supervisor's window carries the login prompt.)
pub(super) const MANIFEST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "messengerd",
        // `soak=4096` drives a boot-time request/reply self-test through the
        // daemon's serve loop and prints `MSGRD:SOAK`/`MSGRD:TOPICS` evidence
        // (issue #169); it costs a fraction of a second and doubles as a
        // liveness check.
        path: fhs::bin::MESSENGERD,
        args: "soak=4096",
        restart: Restart::Once,
        deps: &[],
    },
    ServiceSpec {
        name: "keyd",
        path: fhs::bin::KEYD,
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The configuration registry (issue #260). It needs `messengerd` for the
    // change-topic broker; the store lives on the kernel VFS, so no service
    // dependency. Started before config consumers; `demo=1` spawns one
    // `confctl` self-test that proves the set/get/list/delete path over the
    // real Messenger transport and prints `CONFCTL:SELFTEST:PASS`.
    ServiceSpec {
        name: "confd",
        path: fhs::bin::CONFD,
        args: "demo=1",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The time-of-day service (issue #369): UTC from the kernel wall clock,
    // the zone from `confd` (`sys/time/zone`), and the retained `time/tick`
    // topic on the broker, so it needs both. `demo=1` drives a zone change
    // through `confd` and prints `TIMED:DEMO:PASS` once the service followed.
    ServiceSpec {
        name: "timed",
        path: fhs::bin::TIMED,
        args: "demo=1",
        restart: Restart::Always,
        deps: &["messengerd", "confd"],
    },
    // The input policy service (docs/input-plan.md): the one task holding the
    // kernel `input.raw` capability. It turns the raw HID-coded key edges into
    // layout-aware key events, modifier/lock state and key repeat, so it starts
    // with `confd` (the layout is `sys/input/layout`). `trace=1` echoes decoded
    // events to serial as boot evidence; the desktop profile stays quiet.
    ServiceSpec {
        name: "inputd",
        path: fhs::bin::INPUTD,
        args: if BOOT_EVIDENCE { "trace=1" } else { "" },
        restart: Restart::Always,
        deps: &["confd"],
    },
    ServiceSpec {
        name: "accountsd",
        path: fhs::bin::ACCOUNTSD,
        args: "",
        restart: Restart::Always,
        deps: &[],
    },
    // Administrator-approved operations (docs/accounts-plan.md U2): it
    // checks passwords through `accountsd` and audits on the broker.
    ServiceSpec {
        name: "elevd",
        path: fhs::bin::ELEVD,
        args: "",
        restart: Restart::Always,
        deps: &["accountsd", "messengerd"],
    },
    // The login prompt reads its keys through `inputd`'s console session
    // (issue #396), so it starts once `inputd` serves; it reads the login
    // kind (`sys/session/mode`, graphical or console) from `confd` (#623).
    ServiceSpec {
        name: "logind",
        path: fhs::bin::LOGIND,
        args: "",
        restart: Restart::Always,
        deps: &["accountsd", "inputd", "confd"],
    },
    ServiceSpec {
        name: "logd",
        path: fhs::bin::LOGD,
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    ServiceSpec {
        name: "healthd",
        path: fhs::bin::HEALTHD,
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The per-session clipboard service (issue #115). It only needs the
    // kernel's name registry, so it depends on nothing. `history=1` is the
    // default one-offer policy; `history=N` keeps up to the service's cap.
    // `demo=1` makes it spawn the two clipboard demo clients (`/system/bin/clipcp`,
    // `/system/bin/clippaste`) at startup; they are evidence programs, not supervised
    // services, so they are spawned and reaped by `clipboardd` instead of
    // adding two more ELF loads to this manifest's boot pass.
    ServiceSpec {
        name: "clipboardd",
        path: fhs::bin::CLIPBOARDD,
        args: "history=1 demo=1",
        restart: Restart::Always,
        deps: &[],
    },
    // `mimed` is the MIME database and open-with registry (issue #116). It
    // depends only on the kernel name registry, which every task can use
    // directly; it talks to `init`'s topic router to publish launch events.
    ServiceSpec {
        name: "mimed",
        path: fhs::bin::MIMED,
        args: "",
        restart: Restart::Always,
        deps: &[],
    },
    // The application package manager (docs/packages.md phase 3). It records
    // installed apps in `confd`, registers their file types with `mimed` and
    // loads their Messenger policy into the kernel, so it needs both running.
    // It inherits this supervisor's identity (root, `CAP_IPC_CONTROL`): that is
    // the privilege `acl_load` needs, see `user/src/bin/pkgd.rs`. `Always`
    // because it also restarts itself, on purpose, to give back heap the user
    // allocator never returns.
    ServiceSpec {
        name: "pkgd",
        path: fhs::bin::PKGD,
        args: "",
        restart: Restart::Always,
        deps: &["confd", "mimed"],
    },
    ServiceSpec {
        name: "flaky",
        path: fhs::bin::FLAKY,
        args: "",
        restart: Restart::OnFailure,
        deps: &["healthd"],
    },
    // The sound card driver, at boot only without `devd` (otherwise `devd`
    // asks for it once it found the card, `drivers.rs`).
    #[cfg(all(lazyos_sound, not(lazyos_devd)))]
    SNDD_ROW,
    // The system mixer (docs/audio-plan.md): every application's sound goes
    // through it to the card. It waits for the card on its own (and outlives
    // a driver restart), so it has no start dependency on `sndd`. `demo=1`
    // runs the harness's evidence clients once a card is attached.
    #[cfg(lazyos_sound)]
    ServiceSpec {
        name: "audiod",
        path: fhs::bin::AUDIOD,
        args: "demo=1",
        restart: Restart::Always,
        deps: &[],
    },
    // The USB HID driver (docs/usb-hid-plan.md U2), present only on
    // `LAZYOS_USB=1` images. A machine without an xHCI controller makes it
    // exit cleanly, so `OnFailure` restarts it only after a crash.
    #[cfg(lazyos_usb)]
    ServiceSpec {
        name: "usbd",
        path: fhs::bin::USBD,
        args: USBD_ARGS,
        restart: Restart::OnFailure,
        deps: &["inputd"],
    },
    // The NIC driver, at boot only without `devd` (as `sndd` above).
    #[cfg(all(lazyos_net, not(lazyos_devd)))]
    NETDRV_ROW,
    // The device manager (issue #497): matches the enumerated devices
    // against its driver manifest and asks this supervisor to start each
    // driver for the device it found (`StartDriver`, `drivers.rs`). It needs
    // the broker for its retained `system/devices/<id>` topics.
    #[cfg(lazyos_devd)]
    ServiceSpec {
        name: "devd",
        path: fhs::bin::DEVD,
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The network stack service (docs/networking-plan.md N2), present only on
    // `LAZYOS_NETD=1` images: smoltcp over the NIC driver's rings, as `_netd`
    // with no capabilities. It finds the driver by name and retries until the
    // driver is there (or forever, if the machine has no NIC), so it has no
    // start dependency. `demo=1` runs `netctl`, `ping`, the hostile-input probe
    // and the soak as real clients.
    #[cfg(lazyos_netd)]
    ServiceSpec {
        name: "netd",
        path: fhs::bin::NETD,
        args: NETD_ARGS,
        restart: Restart::Always,
        deps: &[],
    },
    // The network mount service (docs/smb-plan.md §3.4): starts `ftpfuse`
    // for the Network Drives app. The daemons wait for `netd` themselves.
    #[cfg(lazyos_netd)]
    ServiceSpec {
        name: "mountd",
        path: fhs::bin::MOUNTD,
        args: "",
        restart: Restart::Always,
        deps: &["netd"],
    },
    // The system monitor (issue #144): `sysmond` wraps the kernel's
    // system-stats syscall as `os.lazy.system.v1` and republishes retained
    // `system/stats/*` topics. It needs only the kernel name registry, like
    // `mimed`. `demo=1` makes it spawn `top` (`/system/bin/top`), its one-shot
    // evidence client, so a headless boot records `SYS:TOP:PASS`. `top` exits
    // as soon as it has printed its verdict, so it is not a service: listed
    // here it would sit `stopped` and `healthd` would report it `down`
    // forever. `sysmond` spawns and reaps it instead, like `clipboardd`'s demo
    // pair.
    ServiceSpec {
        name: "sysmond",
        path: fhs::bin::SYSMOND,
        args: "demo=1",
        restart: Restart::Always,
        deps: &[],
    },
    // The print spooler (docs/printing-plan.md P6) on desktop images with the
    // network stack, the only ones that ship it: it keeps apps' documents in
    // `fhs::state::PRINT_SPOOL` until their printer has them, so a job
    // outlives the app that printed it. A static musl program (`LINUX_ROWS`),
    // with this supervisor's identity like the platform services. Its
    // printers are reached through `netd`, but it serves without it: a job
    // just waits, or fails to connect.
    #[cfg(all(lazyos_desktop, lazyos_netd))]
    ServiceSpec {
        name: "printd",
        path: fhs::bin::PRINTD,
        args: "",
        restart: Restart::Always,
        deps: &[],
    },
];

/// The sound card driver (docs/driver-plan.md D6, D7), present only on
/// `LAZYOS_SOUND=1` images: virtio-sound or Intel HDA. It needs nothing but the
/// device syscall. `demo=1` plays a test tone that the sound harness records
/// and checks; a machine without the device makes it exit cleanly, so
/// `OnFailure` restarts it only after a real crash.
#[cfg(lazyos_sound)]
pub(super) const SNDD_ROW: ServiceSpec = ServiceSpec {
    name: "sndd",
    path: fhs::bin::SNDD,
    args: "demo=1",
    restart: Restart::OnFailure,
    deps: &[],
};

/// The NIC driver (docs/networking-plan.md N1, issue #497), present only on
/// `LAZYOS_NET=1` images: virtio-net or an Intel 8254x. It needs nothing but
/// the device syscall (and `confd`, softly). `demo=1` runs the ARP self-test
/// and the `nicctl` evidence clients that the network harness checks against
/// the packet capture. A machine without the device makes it idle, so
/// `Always` only restarts it after a crash or a restart-class setting change.
#[cfg(lazyos_net)]
pub(super) const NETDRV_ROW: ServiceSpec = ServiceSpec {
    name: "netdrv",
    path: fhs::bin::NETDRV,
    args: NET_ARGS,
    restart: Restart::Always,
    deps: &[],
};

/// Manifest rows that are static musl programs, spawned under the Linux
/// personality like the desktop's apps.
pub(super) const LINUX_ROWS: &[&str] = &["printd"];

// The runtime rows (`Phase`, `Service`) live in `service.rs`; re-exported so
// the supervisor modules keep one import path.
pub(super) use super::service::{Phase, Service};

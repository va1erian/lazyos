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

/// The `sndd` driver's identity (docs/driver-plan.md D3): a dedicated system
/// uid holding only `CAP_DEV_CLAIM`, so a compromised driver has the device it
/// claimed and nothing else: no `CAP_SETUID`, no path to uid 0.
#[cfg(lazyos_sound)]
const SND_CRED: SysCred = SysCred::new(SND_UID, SND_UID, user::dev::CAP_DEV_CLAIM, 0, 0);
/// The `_snd` system user.
#[cfg(lazyos_sound)]
const SND_UID: u32 = sndpolicy::SND_UID;

/// The mixer's identity (docs/audio-plan.md): its own system uid and **no
/// capabilities at all**. It maps the rings clients hand it and owns the
/// card's stream; it has no device, no DMA and no authority over anyone.
#[cfg(lazyos_sound)]
const AUDIO_CRED: SysCred = SysCred::new(sndpolicy::AUDIO_UID, sndpolicy::AUDIO_UID, 0, 0, 0);

/// `usbd`'s arguments. `trace=1` echoes every report and key edge on serial,
/// which would put typed passwords on the console, so only the USB harness's
/// test images (`LAZYOS_USB_TRACE=1`, `tools/usb/run.py`) turn it on.
#[cfg(lazyos_usb)]
const USBD_ARGS: &str = if cfg!(lazyos_usb_trace) {
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
const USB_CRED: SysCred = SysCred::new(
    USB_UID,
    USB_UID,
    user::dev::CAP_DEV_CLAIM | user::sys::CAP_INPUT_SOURCE | user::sys::CAP_BLOCK_PROVIDER,
    0,
    0,
);
/// The `_usb` system user (901 `_snd`, 902 `_net`, 903 `_netd`).
#[cfg(lazyos_usb)]
const USB_UID: u32 = usbpolicy::USB_UID;

/// The `netdrv` driver's identity (docs/networking-plan.md N1): a dedicated
/// system uid holding only `CAP_DEV_CLAIM`, exactly like `sndd`'s.
#[cfg(lazyos_net)]
const NET_CRED: SysCred = SysCred::new(NET_UID, NET_UID, user::dev::CAP_DEV_CLAIM, 0, 0);
/// The `_net` system user.
#[cfg(lazyos_net)]
const NET_UID: u32 = netpolicy::NET_UID;

/// The argument string of the `netdrv` row: `demo=1` runs the self-test and the
/// evidence clients; `LAZYOS_NET_ARGS` overrides it at build time (the network
/// harness's `--poll` passes `demo=1 irq=poll`).
#[cfg(lazyos_net)]
const NET_ARGS: &str = match option_env!("LAZYOS_NET_ARGS") {
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
const NETD_ARGS: &str = match option_env!("LAZYOS_NETD_ARGS") {
    Some(args) => args,
    None => "demo=1",
};

/// The `netd` stack service's identity (docs/networking-plan.md N2): its own
/// system uid and **no capabilities at all**: it holds no device authority, no
/// DMA, nothing it could misuse if a parser bug handed an attacker the process.
#[cfg(lazyos_netd)]
const NETD_CRED: SysCred = SysCred::new(NETD_UID, NETD_UID, 0, 0, 0);
/// The `_netd` system user.
#[cfg(lazyos_netd)]
const NETD_UID: u32 = netpolicy::NETD_UID;

/// Credentials a manifest row is spawned with; `None` inherits this
/// supervisor's identity, which is what the platform services need.
pub(super) fn manifest_cred(name: &str) -> Option<SysCred> {
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
    ServiceSpec {
        name: "logind",
        path: fhs::bin::LOGIND,
        args: "",
        restart: Restart::Always,
        deps: &["accountsd"],
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
    // The virtio-sound userspace driver (docs/driver-plan.md D6), present only
    // on `LAZYOS_SOUND=1` images. It needs nothing but the device syscall.
    // `demo=1` plays a test tone that the sound harness records and checks;
    // a machine without the device makes it exit cleanly, so `OnFailure`
    // restarts it only after a real crash.
    #[cfg(lazyos_sound)]
    ServiceSpec {
        name: "sndd",
        path: fhs::bin::SNDD,
        args: "demo=1",
        restart: Restart::OnFailure,
        deps: &[],
    },
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
    // The virtio-net userspace driver (docs/networking-plan.md N1), present only
    // on `LAZYOS_NET=1` images. It needs nothing but the device syscall (and
    // `confd`, softly). `demo=1` runs the ARP self-test and the `nicctl`
    // evidence clients that the network harness checks against the packet
    // capture. A machine without the device makes it idle, so `Always` only
    // restarts it after a crash or a restart-class setting change.
    #[cfg(lazyos_net)]
    ServiceSpec {
        name: "netdrv",
        path: fhs::bin::NETDRV,
        args: NET_ARGS,
        restart: Restart::Always,
        deps: &[],
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

/// Manifest rows that are static musl programs, spawned under the Linux
/// personality like the desktop's apps.
pub(super) const LINUX_ROWS: &[&str] = &["printd"];

// The runtime rows (`Phase`, `Service`) live in `service.rs`; re-exported so
// the supervisor modules keep one import path.
pub(super) use super::service::{Phase, Service};

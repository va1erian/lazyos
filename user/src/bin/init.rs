//! `init` (`/system/bin/init`): the userspace service supervisor (issue #93) and the
//! app-launch path (issue #158).
//!
//! This is the S2 supervisor from `docs/messenger.md` section 8 and the
//! platform plan section 4.3. It boots as a normal ring-3 program and owns the
//! system from there:
//!
//! * it registers [`services::INIT_NAME`] and serves its supervision table
//!   (`Services`), its built-in app registry (`ListApps`) and its launch call
//!   (`Launch`) plus a [`router::TopicBroker`] on the same endpoint;
//! * it starts services from [`MANIFEST`] in dependency order, through the
//!   native `spawn` syscall (syscall 6), which makes each service a child of
//!   this task;
//! * it waits for child exits with the native `wait` syscall (syscall 7) and
//!   restarts a crashed service after a capped exponential backoff, giving up
//!   after [`MAX_RESTARTS`] rapid crashes;
//! * every state change is published retained on
//!   `system/events/service/<name>`, so `logd` (and any subscriber) sees it,
//!   and dependencies only start once their dependencies are running.
//!
//! # App launch and the registry
//!
//! The built-in [`APPS`] table maps an **app id** (the lowercase program stem,
//! `top` -> `/system/bin/top`) to a display name, its ELF path, a default restart
//! policy and the MIME verbs it handles. `ListApps` serves it to the S5 start
//! menu, and `mimed`'s open-with registrations resolve to the same ids.
//!
//! `Launch(app_id, args, session)` spawns *the target session's child* with
//! `spawnv` and a credential stamp, so the kernel stamps uid/gid/session
//! before the app runs, and
//! then supervises it exactly like a manifest service: the same restart policy,
//! crash backoff, `system/health/<name>` and `system/events/service/<name>`.
//! `args` is empty or one absolute path, appended after the row's fixed
//! arguments as a single `argv` item (`launch::launch_argument`; anything else
//! is `-EINVAL`). `session` 0 means the caller's own session. The policy is session-owner
//! only: a task may launch into its own session; root (or a task holding
//! `CAP_SETUID`) may launch anywhere; anyone else is refused with `-EPERM`
//! (printed as `INIT:LAUNCH:DENIED:PASS` and answered with a structured
//! error). Root launching into a *different* session resolves the session's
//! uid/gid from `logind`.
//!
//! A session may hold at most [`LAUNCH_CAP_PER_SESSION`] launched rows
//! reserved at once (issue #177): each `Launch` call spawns a fresh row and
//! only a `Stopped`/`Failed` row for the same app is ever superseded, so
//! nothing else stopped an unprivileged caller from looping `launch` until
//! the task table (`kernel/src/task/mod.rs`'s `MAX_TASKS`) was full,
//! starving supervised restarts and new logins. A request over the cap is
//! refused with `-EAGAIN` (`INIT:LAUNCH:CAP:PASS`) before anything spawns.
//! The reservation counts `Running` rows and any row still cycling through
//! crash backoff (`Restarting`/`Pending`), since those respawn from the
//! supervision loop without another cap check ([`running_in_session`]).
//!
//! Boot evidence: `INIT:APPS:PASS`, `INIT:LAUNCH:PASS` (the self-test launches
//! `/system/bin/top`; the app's own `SYS:TOP:PASS` and exit prove it ran),
//! `INIT:LAUNCH:DENIED:PASS` (the policy self-test) and `INIT:LAUNCH:CAP:PASS`
//! (the concurrency-cap self-test); supervised restarts print
//! `INIT:RESTART:PASS`.
//!
//! The manifest is a static Rust table today. Each row carries the fields the
//! issue asks for: name, program path, argument string, restart policy, dependency
//! names and health topic. The supervisor appends `attempt=<n>` to the
//! argument string on every spawn, so a service can distinguish a restart; that
//! is how `/system/bin/flaky` crashes exactly once.
//!
//! # Shutdown and reboot
//!
//! `Shutdown(mode, reason, force)` is the only way the machine stops: `init`
//! stops the session apps, then the services in reverse dependency order
//! (`stop_order`), and calls the kernel's `power` last (`shutdown`;
//! docs/shutdown.md). From the request on, nothing restarts and `Launch` is
//! refused with `-EBUSY`.
//!
//! Boot it with `LAZYOS_SERVICES=1` (see the kernel build script).

#![no_std]
#![no_main]

extern crate alloc;

#[path = "init/apps.rs"]
mod apps;
#[path = "init/autostart.rs"]
mod autostart;
#[path = "init/home.rs"]
mod home;
#[path = "init/installed.rs"]
mod installed;
#[path = "init/launch.rs"]
mod launch;
#[path = "init/protocol.rs"]
mod protocol;
#[path = "init/provisioning.rs"]
mod provisioning;
#[path = "init/selftest.rs"]
mod selftest;
#[path = "init/service.rs"]
mod service;
#[path = "init/sessions.rs"]
mod sessions;
#[path = "init/shutdown.rs"]
mod shutdown;
#[path = "init/state.rs"]
mod state;
#[path = "init/stop.rs"]
mod stop;
#[path = "init/stop_order.rs"]
mod stop_order;
#[path = "init/supervise.rs"]
mod supervise;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, registry, router, services};
use user::sys;

use autostart::Autostart;
use installed::InstalledApps;
use protocol::{serve_pending, StatusCache, Supervisor};
use selftest::{
    selftest_launch_args, selftest_launch_cap, selftest_launch_policy, selftest_shell_supervision,
    LaunchSelftest,
};
use state::{Phase, Service, BOOT_EVIDENCE, BOOT_SELFTESTS, MANIFEST};
use supervise::{child_exited, spawn_service, start_ready, wake_deadline};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("init: supervisor starting (issue #93)\n");
    if let Err(error) = run() {
        sys::write_str("init: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the supervisor, start the manifest, and run the supervision loop.
fn run() -> messenger::Result<()> {
    // The endpoint clients resolve: it serves the topic broker and the
    // supervision table. Registering the published side and serving the other
    // is the same split `Bus::subscribe` uses for a sink.
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::INIT_NAME,
        &published,
        &[services::INIT_INTERFACE, router::INTERFACE],
        0,
    )?;
    let mut broker = router::TopicBroker::new("os.lazy.events.sink");
    // Release and desktop boots skip the evidence-only rows (`flaky`'s
    // deliberate crashes).
    let mut services: Vec<Service> = MANIFEST
        .iter()
        .filter(|spec| BOOT_EVIDENCE || spec.name != "flaky")
        .map(Service::from_manifest)
        .collect();
    sys::write_str(&format!("init: manifest: {} service(s)\n", services.len()));
    apps::load();
    if BOOT_SELFTESTS {
        // The packaged apps are checked once `pkgd` provisioned them
        // (`autostart`); the built-ins can be checked now.
        if !apps::selftest_builtins() {
            sys::write_str("INIT:APPS:FAIL the built-in registry is malformed\n");
        }
        selftest_launch_policy();
        selftest_launch_cap();
        selftest_launch_args();
        selftest_shell_supervision();
        sys::write_str(sessions::selftest());
        stop_order::selftest_stop_order();
    }
    start_ready(&mut services, &mut broker);
    // One receive buffer for the whole life of the supervisor: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut cache = StatusCache::default();
    let mut selftest = LaunchSelftest::new();
    let mut autostart = Autostart::new();
    let mut installed = InstalledApps::new();
    // Set by a `Shutdown` request; from then on nothing starts or restarts and
    // the loop steps the shutdown instead (docs/shutdown.md).
    let mut shutdown: Option<shutdown::Shutdown> = None;

    loop {
        let now = sys::clock();
        if shutdown.is_none() {
            // A restart whose backoff elapsed.
            for index in 0..services.len() {
                if services[index].phase == Phase::Restarting && services[index].next_start <= now {
                    spawn_service(&mut services, index, &mut broker);
                }
            }
            // The boot launch self-test: spawn `/system/bin/top` through the real
            // launch path once a task slot is free (the manifest's one-shot
            // `top` exits around here), proving `Launch` end to end in a
            // headless boot.
            if BOOT_EVIDENCE {
                selftest.step(&mut services, &mut broker, now);
            }
            // A home volume on a USB stick (`home`): once it is mounted, or
            // the bounded wait for it ends, start what was held for it.
            if home::step(&services, now) {
                start_ready(&mut services, &mut broker);
            }
            // The desktop's apps (issue #215): open the shipped `autostart` rows.
            if home::ready() {
                autostart.step(&mut services, &mut broker, &mut installed, now);
            }
        }
        // Reap one exit (or time out to serve requests).
        if let Some((pid, status)) = sys::wait(home::wake(wake_deadline(&services, now), now)) {
            child_exited(&mut services, pid, status, &mut broker);
            // The exit may unblock dependents (only a stop can; still cheap).
            if shutdown.is_none() {
                start_ready(&mut services, &mut broker);
            }
        }
        // A shutdown accepted in this pass starts stepping in the next one:
        // the park in between lets the requester run and see its reply before
        // `init` signals the apps, the requester (LazyShell) among them. A
        // same-class wake does not preempt, so stepping at once would stop it
        // with the reply unread.
        let stepping = shutdown.is_some();
        serve_pending(
            &mut Supervisor {
                services: &mut services,
                broker: &mut broker,
                installed: &mut installed,
                cache: &mut cache,
                shutdown: &mut shutdown,
            },
            &server,
            &mut buffer,
        )?;
        if let Some(running) = shutdown.as_mut().filter(|_| stepping) {
            running.step(&mut services, &mut broker);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}

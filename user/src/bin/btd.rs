//! `btd` (`/system/bin/btd`): the Bluetooth host service
//! (docs/bluetooth-plan.md B3). `init` starts it at boot as `_bt` (uid 916)
//! holding only `CAP_INPUT_SOURCE`: no device claim, no DMA, no read of the
//! input bus. It waits for `usbd` to serve `os.lazy.usb.bt`, attaches its
//! rings, and runs the host stack against the controller.
//!
//! This file is the I/O shell. The decisions live in `libs/bthost` (a sans-IO
//! state machine tested against `libs/btsim`): bring-up, who is admitted,
//! pairing, the bond store's contents, HID. The shell feeds it what arrives,
//! carries out what it asks (commands and ACL packets to the controller, input
//! sources to the kernel, the bond file to disk, evidence lines to serial) and
//! sleeps on the transport's doorbell until the host's next deadline: nothing
//! here polls on a timer (docs/bluetooth-plan.md decision 11).
//!
//! The bond file is read before the controller is brought up
//! (`BT:BOND:LOAD n=`). Boot arguments (`trace=1`, `pair=...`) are passed by
//! `init` only in harness builds (`LAZYOS_BT_TRACE=1`); a normal build gets
//! none, so nobody can pair and no report is echoed.
//!
//! Serial evidence: `BT:CRED`, `BT:BOND:LOAD`, `BT:WAIT` (no controller yet),
//! `BT:LINK` (attached), `BT:READY ms=` (the fast path is done: a bonded
//! device can connect), `BT:CTRL:PASS` (brought up) or `BT:CTRL:FAIL`,
//! `BT:CONN`, `BT:ENCRYPT`, `BT:HID:KBD|MOUSE`, `BT:RECONNECT ms=`,
//! `BT:PAIR:*`, `BT:REFUSE reason=`, `BT:CTRL:GONE`. Addresses appear as the
//! last two octets only and no key ever does. LE devices (B4) use the same
//! lines (`BT:CONN ... role=le`, `BT:PAIR:START`, `BT:ENCRYPT`, `BT:HID:*`,
//! `BT:RECONNECT`), plus `BT:GATT:FAIL`.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use bthost::store::{self, Loaded};
use bthost::{Config, Effect, Host};
use user::sys;
use user::trace::{self, Channel};

#[path = "btd/disk.rs"]
mod disk;
#[path = "btd/firmware.rs"]
mod firmware;
#[path = "btd/link.rs"]
mod link;
#[path = "btd/sources.rs"]
mod sources;

use link::{deadline_ticks, Failure, Frame, Link};
use sources::Sources;

/// Ticks (100 Hz) between looks for a controller while none is bound. `init`
/// starts `btd` after `usbd` served the controller, so a boot never waits
/// here; only a dongle plugged in later does (Messenger has no registry
/// watch to park on instead).
const WAIT_TICKS: u64 = 100;
/// The longest park with nothing due; a doorbell wakes it earlier.
const IDLE_TICKS: u64 = 6000;
/// Milliseconds to wait for room when the command ring is full: the only
/// timed wake that is not a deadline, and only while `usbd` is not reading.
const BACKLOG_RETRY_MS: u64 = 10;
/// Frames handled between looks at the host's queues, so a flood of events
/// cannot starve the commands that answer them.
const BATCH: usize = 32;

/// The service arguments `init` passed.
fn args() -> Config {
    let mut buf = [0u8; 192];
    let len = sys::service_args(&mut buf);
    let text = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
    Config::from_args(text)
}

/// The `attempt=<n>` `init` appends to every spawn (1 for the first).
fn attempt() -> u32 {
    let mut buf = [0u8; 192];
    let len = sys::service_args(&mut buf);
    let text = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
    text.split_whitespace()
        .find_map(|arg| arg.strip_prefix("attempt=")?.parse().ok())
        .unwrap_or(1)
}

/// Read the bond file (before any controller exists) and say what was there.
fn load_bonds() -> btbond::BondStore {
    let (bonds, loaded) = store::load(&mut disk::Disk, &disk::PATHS);
    match loaded {
        Loaded::Bonds(n) => trace::line!(Channel::Bt, "BOND:LOAD n={n}"),
        Loaded::Missing => trace::line!(Channel::Bt, "BOND:LOAD n=0 file=missing"),
        Loaded::Damaged { moved } => {
            trace::line!(
                Channel::Bt,
                "BOND:LOAD n=0 file=damaged moved_aside={moved}"
            )
        }
        Loaded::Unreadable(errno) => {
            trace::line!(Channel::Bt, "BOND:LOAD n=0 file=unreadable errno={errno}")
        }
    }
    bonds
}

/// Wait for `usbd` to bind a controller, then attach the rings.
fn open() -> Link {
    let mut logged = false;
    loop {
        match Link::open() {
            Ok(link) => return link,
            Err("no controller") => {
                if !logged {
                    trace::line!(Channel::Bt, "WAIT no Bluetooth controller yet");
                    logged = true;
                }
            }
            Err(why) => trace::line!(Channel::Bt, "LINK:FAIL {why}"),
        }
        sys::wait(sys::clock() + WAIT_TICKS);
    }
}

/// Write the bond file; a failure is tried once more (the file layer cleans
/// up after itself, and a later save writes the whole store again anyway).
fn save_bonds(bytes: &[u8]) {
    for attempt in 1..=2 {
        match store::save(&mut disk::Disk, &disk::PATHS, bytes) {
            Ok(()) => return trace::line!(Channel::Bt, "BOND:SAVE bytes={}", bytes.len()),
            Err(failure) => trace::line!(
                Channel::Bt,
                "BOND:SAVE:FAIL step={} errno={} attempt={attempt}",
                failure.step,
                failure.errno
            ),
        }
    }
}

/// The `BT:REFUSE` lines allowed in a window: a stranger in radio range can
/// knock as often as it likes, and the console is not its to fill.
static REFUSALS: tracefmt::Budget = tracefmt::Budget::new(16, 6000);

/// Carry out what the host decided; returns false once it gave up on the
/// controller.
fn act(effect: Effect, sources: &mut Sources) -> bool {
    match effect {
        Effect::OpenSource { handle, kind } => sources.open(handle, kind),
        Effect::Input { handle, event } => sources.publish(handle, event),
        Effect::CloseSources { handle } => sources.close(handle),
        Effect::SaveBonds(bytes) => save_bonds(&bytes),
        Effect::Trace(text) => {
            if !text.starts_with("REFUSE") || trace::within(&REFUSALS, Channel::Bt) {
                trace::line!(Channel::Bt, "{text}");
            }
        }
        Effect::Failed(why) => {
            trace::fail!(Channel::Bt, "CTRL", "{why}");
            return false;
        }
        // Answered where the host is at hand (`drain`).
        Effect::LoadFirmware { .. } => {}
    }
    true
}

/// Seed the host's pairing entropy from the kernel (`random`, syscall 26).
/// LE Secure Connections makes P-256 keys and passkeys from it
/// (docs/bluetooth-plan.md decision 8); if the kernel gives none the host
/// stays unseeded and refuses to pair rather than make guessable keys.
fn seed_entropy(host: &mut Host) {
    let mut seed = [0u8; 32];
    match sys::random(&mut seed) {
        Ok(()) => host.seed_entropy(&seed),
        Err(errno) => trace::line!(Channel::Bt, "ENTROPY:FAIL errno={errno}"),
    }
    host.seed_entropy(&sys::monotonic_ms().to_le_bytes());
}

/// Everything the host has to say, to the controller and the world.
fn drain(
    host: &mut Host,
    link: &mut Link,
    sources: &mut Sources,
    now: u64,
) -> Result<bool, Failure> {
    let mut alive = true;
    loop {
        let mut progress = false;
        while let Some(packet) = host.poll_command(now) {
            link.send_command(&packet)?;
            progress = true;
        }
        while let Some(packet) = host.poll_acl() {
            link.send_acl(&packet)?;
            progress = true;
        }
        while let Some(effect) = host.poll_effect() {
            progress = true;
            if let Effect::LoadFirmware { name, .. } = &effect {
                host.on_firmware_file(sys::monotonic_ms(), firmware::read(name));
                continue;
            }
            alive &= act(effect, sources);
        }
        if !progress {
            return Ok(alive);
        }
    }
}

/// Run the host against an attached controller until it goes away.
/// `reenumerations` is the count of times the controller reset itself during
/// this bring-up, kept in `btd`'s memory across the re-attachments (the host
/// stops after a few, `bthost::MAX_REENUMERATIONS`).
fn serve(link: &mut Link, sources: &mut Sources, reenumerations: &mut u32) -> Failure {
    let ids = link.usb_ids();
    let bonds = load_bonds();
    trace::span(Channel::Bt, "bonds-loaded");
    let mut host = Host::new_for(args(), bonds, sys::monotonic_ms(), ids, *reenumerations);
    seed_entropy(&mut host);
    let failure = run(&mut host, link, sources);
    if matches!(failure, Failure::Gone) {
        // A download in progress hears of the departure (it may be the
        // expected re-enumeration) and says what it makes of it.
        host.on_detached(sys::monotonic_ms());
        while let Some(effect) = host.poll_effect() {
            act(effect, sources);
        }
    }
    *reenumerations = host.firmware_attempts();
    failure
}

/// The loop of [`serve`]: feed the host, carry out what it asks, sleep.
fn run(host: &mut Host, link: &mut Link, sources: &mut Sources) -> Failure {
    let mut alive = true;
    loop {
        let now = sys::monotonic_ms();
        let mut handled = 0;
        while handled < BATCH {
            match link.next_frame() {
                Ok(Some(Frame::Event(packet))) => host.on_event(now, &packet),
                Ok(Some(Frame::Acl(packet))) => host.on_acl(now, &packet),
                Ok(None) => break,
                Err(failure) => return failure,
            }
            handled += 1;
        }
        if host.next_deadline().is_some_and(|due| due <= now) {
            host.on_tick(now);
        }
        match drain(host, link, sources, now) {
            Ok(still) => alive = still && alive,
            Err(failure) => return failure,
        }
        if let Err(failure) = link.flush() {
            return failure;
        }
        if handled == BATCH {
            continue;
        }
        // Sleep until the host's next deadline, or the doorbell.
        let mut due = host.next_deadline();
        if link.backlogged() {
            let retry = now + BACKLOG_RETRY_MS;
            due = Some(due.map_or(retry, |d| d.min(retry)));
        }
        let ticks = match (alive, due) {
            (true, Some(due)) => deadline_ticks(now, due),
            _ => sys::clock() + IDLE_TICKS,
        };
        if let Err(failure) = link.sleep(ticks) {
            return failure;
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    trace::span(Channel::Bt, "start");
    match sys::cred_get(None) {
        Ok(cred) => trace::line!(Channel::Bt, "CRED uid={} caps={:#x}", cred.uid, cred.caps),
        Err(errno) => trace::line!(Channel::Bt, "CRED unavailable (errno {errno})"),
    }
    // Times the controller reset itself during this bring-up (see `serve`).
    let mut reenumerations = 0;
    loop {
        let mut link = open();
        trace::span(Channel::Bt, "attached");
        trace::line!(Channel::Bt, "LINK ring={}", link.ring());
        let mut sources = Sources::new(
            args().trace_reports,
            cfg!(lazyos_bt_crash_test) && attempt() == 1,
        );
        let failure = serve(&mut link, &mut sources, &mut reenumerations);
        // Whatever the devices held is released with their sources.
        sources.close_all();
        match failure {
            Failure::Gone => trace::line!(Channel::Bt, "CTRL:GONE the controller was unplugged"),
            Failure::Broken(why) => trace::fail!(Channel::Bt, "LINK", "{why}"),
        }
        let attached_to = link.object();
        link.close();
        // A controller that fails at once every time must not spin: wait for
        // `usbd` to withdraw it before looking again. A controller that left
        // and came back within one look is a new registration, which also
        // ends the wait.
        while attached_to.is_some() && link::transport_object() == attached_to {
            sys::wait(sys::clock() + WAIT_TICKS);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    trace::line!(Channel::Bt, "PANIC {info}");
    sys::exit(2)
}

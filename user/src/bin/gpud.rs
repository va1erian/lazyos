//! `gpud` (`/system/bin/gpud`): the Intel GPU service
//! (docs/kabylake-gpu-plan.md), as far as stage G1's first half: **adopt and
//! observe, read-only**, for the two display engines `libs/intelgfx`
//! decodes (Gen9, display 13).
//!
//! `devd` starts it through `init`'s `gpud` driver row when it finds the Kaby
//! Lake HD Graphics 620 (`8086:5916`) or the Alder Lake-P UHD Graphics
//! (`8086:46b3`) (`libs/devmatch`, only in an image built with
//! `LAZYOS_INTELGFX=1`), with `dev=<id>`, as `_gpu` (uid 915) holding
//! only `CAP_DEV_CLAIM`, which the kernel's class rules narrow to display
//! controllers (`libs/gpupolicy`). It:
//!
//! 1. finds the function and claims it **without** an interrupt channel, and
//!    undoes the claim's quiesce of memory decode and bus mastering so the
//!    kernel console keeps drawing (`device.rs`: `GPU:PCI`);
//! 2. maps BAR 0 and wraps it in a read-only `intelgfx::Mmio` (`bar.rs`:
//!    no write path, reads only inside the ranges `dbgd` may read);
//! 3. reads the display state the firmware left (`intelgfx::adopt`, with the
//!    device's generation: power wells, CDCLK, DPLLs, every pipe with its
//!    transcoder, DDI function and first plane, the GGTT pages of the scanout surface) and prints it
//!    (`report.rs`: `GPU:REG`, `GPU:PIPE`, `GPU:MODE`, `GPU:ADOPT:PASS`, ...);
//! 4. measures the adopted pipe's refresh rate from its frame counter and
//!    reads its FIFO underrun state (`beam.rs`: `GPU:VBLANK`,
//!    `GPU:UNDERRUN`);
//! 5. serves `os.lazy.gpu.v1` (`idl/gpu.midl`) from that snapshot, and the
//!    beam (frame counter, scan line) live (`serve.rs`).
//!
//! It writes no display register, takes no interrupt, programs no mode and
//! touches no engine. The one exception is opt-in: started with `audio=1`
//! it writes the HDMI audio block (stage G2, `audio.rs`), through a grant
//! that names those registers and no others. Pipe CRCs, `set_scanout` and
//! flips are separate changes. Started with no such device (by hand, or a stale `dev=`) it
//! prints `GPU:NO-DEVICE:FAIL` once and exits 0, which `init`'s `OnFailure`
//! row does not restart.
//!
//! **It never exits on purpose while it holds the claim**: a task's death
//! releases the claim, and a release quiesces the function again (memory
//! decode off), which would stop the console's writes into the aperture
//! until `init` restarts it. A failure after the claim prints `GPU:FAIL:<step>`
//! and parks with the claim held; `dbgctl reload gpud` replaces the binary.
//! Arguments that do not parse, or do not fit the read buffer, print
//! `GPU:ARGS:FAIL` and exit 0 before anything is claimed (a restart would read
//! the same words). A failure before the claim of another kind (the device
//! list, the claim: `EBUSY` while the kernel still tears down a reloaded
//! driver's claim) exits 1 for `init`'s `OnFailure` row to try again.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use intelgfx::adopt::{self, Display};
use intelgfx::args::{self, Args, ArgsError};
use user::sys;
use user::trace::{self, Channel};

#[path = "gpud/audio.rs"]
mod audio;
#[path = "gpud/bar.rs"]
mod bar;
#[path = "gpud/beam.rs"]
mod beam;
#[path = "gpud/device.rs"]
mod device;
#[path = "gpud/report.rs"]
mod report;
#[path = "gpud/serve.rs"]
mod serve;

/// Longest park between wake-ups (100 Hz ticks) of the parked state.
const IDLE_TICKS: u64 = 6000;

/// The words `init` started this driver with (`dev=<id>` from `devd`, and
/// `audio=1 edid=<path>` from the image), parsed once. A buffer too short for
/// them, or a bad value, is an error: see `intelgfx::args`.
fn read_args() -> Result<Args, ArgsError> {
    let mut buffer = [0u8; args::BUFFER_BYTES];
    let needed = sys::service_args(&mut buffer);
    args::parse(args::text(&buffer, needed)?)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match sys::cred_get(None) {
        Ok(cred) => trace::line!(Channel::Gpu, "CRED uid={} caps={:#x}", cred.uid, cred.caps),
        Err(errno) => trace::line!(Channel::Gpu, "CRED unavailable (errno {errno})"),
    }
    let args = match read_args() {
        Ok(args) => args,
        Err(why) => {
            // Nothing is claimed yet. A restart would read the same words, so
            // say so once and exit cleanly (`OnFailure` does not restart a
            // clean exit) rather than guess a device or switch audio off.
            trace::fail!(Channel::Gpu, "ARGS", "{why:?}");
            sys::exit(0)
        }
    };
    let wanted = args.dev;
    let row = match device::find(wanted) {
        Ok(Some(row)) => row,
        Ok(None) => {
            // Nothing to drive will become drivable by a restart: say so
            // once and exit cleanly (`OnFailure` does not restart a clean
            // exit). Where QEMU's VGA is the only display function this is
            // the line a boot shows.
            trace::fail!(
                Channel::Gpu,
                "NO-DEVICE",
                "no Intel display function intelgfx knows ({:04x}:{:?}) in the device list \n                 (dev={wanted:?})",
                device::VENDOR,
                devmatch::INTELGFX_DEVICES
            );
            sys::exit(0)
        }
        Err(errno) => {
            // Nothing is touched yet: let `init` try again.
            trace::fail!(Channel::Gpu, "LIST", "errno={errno}");
            sys::exit(1)
        }
    };
    trace::pass!(
        Channel::Gpu,
        "UP",
        "dev={} {:04x}:{:04x} bar0_bytes={}",
        row.id,
        row.vendor,
        row.device,
        row.bar_len[device::BAR]
    );
    let (claimed, command) = match device::claim(row) {
        Ok(claimed) => claimed,
        Err((step, errno)) => {
            // Not claimed (a claim a reloaded driver still holds is `EBUSY`
            // for a moment), or claimed and failed before anything else:
            // the kernel's teardown of this exit releases it.
            trace::fail!(Channel::Gpu, step, "errno={errno}");
            sys::exit(1)
        }
    };
    match adopt_and_serve(&claimed, &command, &args) {
        Err(why) => {
            trace::fail!(Channel::Gpu, why.0, "{}", why.1);
            park()
        }
        // `serve::run` never returns Ok.
        Ok(never) => match never {},
    }
}

/// A failure after the claim: the step and what the kernel or the hardware
/// said.
struct Failure(&'static str, alloc::string::String);

fn step((step, errno): device::StepError) -> Failure {
    Failure(step, alloc::format!("errno={errno}"))
}

fn adopt_and_serve(
    claimed: &device::Claimed,
    command: &device::Command,
    args: &Args,
) -> Result<core::convert::Infallible, Failure> {
    // `device::find` took only a device `Display::for_device` knows.
    let display = Display::for_device(claimed.row.device)
        .ok_or_else(|| Failure("display", alloc::format!("{:04x}", claimed.row.device)))?;
    trace::line!(Channel::Gpu, "DISPLAY engine={}", display.name());
    let identity = device::identity(claimed, display).map_err(step)?;
    report::pci(&identity, command);
    let (base, len) = device::map(claimed).map_err(step)?;
    // SAFETY: `device::map` mapped the whole BAR, `len` bytes, uncached; the
    // mapping lives until the claim ends with this task, which never exits
    // on purpose (the module docs).
    let mut regs = unsafe { bar::Bar::new(base, len, claimed.row.vendor, claimed.row.device) };
    let file = display.read(&mut regs);
    report::registers(&file);
    if file.looks_dead() {
        return Err(Failure(
            "dead",
            alloc::format!(
                "the power well and CDCLK registers read all ones (command {:#06x}): the BAR \
                 is not decoding",
                command.restored
            ),
        ));
    }
    let adopted = display.decode(&file);
    // The scanout surface's GGTT pages: where the framebuffer really is.
    let fb = adopted
        .active
        .filter(|pipe| pipe.plane.enabled)
        .map(|pipe| adopt::read_fb_map(&mut regs, pipe.plane.surf, pipe.plane.bytes()));
    report::adopted(&adopted, fb.as_ref());
    // Stage G2, only when started with `audio=1`: the one place that writes.
    audio::run(args, &mut regs, &adopted);
    trace::line!(
        Channel::Gpu,
        "DONE reads={} writes={} refused_reads={} refused_writes={}",
        regs.reads(),
        regs.writes(),
        regs.refused_reads,
        regs.refused_writes
    );
    let facts = serve::Facts {
        device: claimed.row.device,
        revision: identity.revision,
    };
    let watch = adopted.active.map(|pipe| beam::Watch::start(regs, &pipe));
    serve::run(serve::state(&facts, &adopted, fb.as_ref()), watch)
        .map_err(|why| Failure("serve", alloc::string::String::from(why)))
}

/// Park for good after a failure, the claim held.
fn park() -> ! {
    loop {
        let _ = sys::wait(sys::clock() + IDLE_TICKS);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    trace::fail!(Channel::Gpu, "PANIC", "{}", info.message());
    // Parked, not exited: see the module docs. A panic before the claim
    // cannot hold one, and exiting then would be right, but a driver that
    // panics before it claims anything is a bug `GPU:PANIC` reports either
    // way, and a restart loop would only repeat it.
    park()
}

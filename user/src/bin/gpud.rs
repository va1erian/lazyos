//! `gpud` (`/system/bin/gpud`): the Intel Gen9 GPU service
//! (docs/kabylake-gpu-plan.md), as far as stage G1's first half: **adopt and
//! observe, read-only**.
//!
//! `devd` starts it through `init`'s `gpud` driver row when it finds the Kaby
//! Lake HD Graphics 620 (`8086:5916`, `libs/devmatch`, only in an image built
//! with `LAZYOS_GEN9GFX=1`), with `dev=<id>`, as `_gpu` (uid 915) holding
//! only `CAP_DEV_CLAIM`, which the kernel's class rules narrow to display
//! controllers (`libs/gpupolicy`). It:
//!
//! 1. finds the function and claims it **without** an interrupt channel, and
//!    undoes the claim's quiesce of memory decode and bus mastering so the
//!    kernel console keeps drawing (`device.rs`: `GPU:PCI`);
//! 2. maps BAR 0 and wraps it in a read-only `gen9gfx::Mmio` (`bar.rs`:
//!    no write path, reads only inside the ranges `dbgd` may read);
//! 3. reads the display state the firmware left (`gen9gfx::adopt`: power
//!    wells, CDCLK, DPLLs, pipes A to C with transcoder, DDI function and
//!    first plane, the GGTT pages of the scanout surface) and prints it
//!    (`report.rs`: `GPU:REG`, `GPU:PIPE`, `GPU:MODE`, `GPU:ADOPT:PASS`, ...);
//! 4. measures the adopted pipe's refresh rate from its frame counter and
//!    reads its FIFO underrun state (`beam.rs`: `GPU:VBLANK`,
//!    `GPU:UNDERRUN`);
//! 5. serves `os.lazy.gpu.v1` (`idl/gpu.midl`) from that snapshot, and the
//!    beam (frame counter, scan line) live (`serve.rs`).
//!
//! It writes no display register, takes no interrupt, programs no mode and
//! touches no engine: pipe CRCs, `set_scanout`, flips and G2 onwards are
//! separate changes. Started with no such device (by hand, or a stale `dev=`) it
//! prints `GPU:NO-DEVICE:FAIL` once and exits 0, which `init`'s `OnFailure`
//! row does not restart.
//!
//! **It never exits on purpose while it holds the claim**: a task's death
//! releases the claim, and a release quiesces the function again (memory
//! decode off), which would stop the console's writes into the aperture
//! until `init` restarts it. A failure after the claim prints `GPU:FAIL:<step>`
//! and parks with the claim held; `dbgctl reload gpud` replaces the binary.
//! A failure before it (the device list, the claim: `EBUSY` while the kernel
//! still tears down a reloaded driver's claim) exits 1 for `init`'s
//! `OnFailure` row to try again.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use gen9gfx::adopt::{self, Adopted, RegFile};
use user::sys;
use user::trace::{self, Channel};

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

/// `dev=<id>`: the device `devd` matched this driver to.
fn device_arg() -> Option<u64> {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace()
        .find_map(|part| part.strip_prefix("dev="))
        .and_then(|id| id.parse().ok())
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match sys::cred_get(None) {
        Ok(cred) => trace::line!(Channel::Gpu, "CRED uid={} caps={:#x}", cred.uid, cred.caps),
        Err(errno) => trace::line!(Channel::Gpu, "CRED unavailable (errno {errno})"),
    }
    let wanted = device_arg();
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
                "no Intel HD Graphics 620 ({:04x}:{:04x}) in the device list (dev={wanted:?})",
                device::VENDOR,
                device::DEVICE
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
    match adopt_and_serve(&claimed, &command) {
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
) -> Result<core::convert::Infallible, Failure> {
    let identity = device::identity(claimed).map_err(step)?;
    report::pci(&identity, command);
    let (base, len) = device::map(claimed).map_err(step)?;
    // SAFETY: `device::map` mapped the whole BAR, `len` bytes, uncached; the
    // mapping lives until the claim ends with this task, which never exits
    // on purpose (the module docs).
    let mut regs =
        unsafe { bar::ReadOnlyBar::new(base, len, claimed.row.vendor, claimed.row.device) };
    let file = RegFile::read(&mut regs);
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
    let adopted = Adopted::decode(&file);
    // The scanout surface's GGTT pages: where the framebuffer really is.
    let fb = adopted
        .active
        .filter(|pipe| pipe.plane.enabled)
        .map(|pipe| adopt::read_fb_map(&mut regs, pipe.plane.surf, pipe.plane.bytes()));
    report::adopted(&adopted, fb.as_ref());
    trace::line!(
        Channel::Gpu,
        "DONE reads={} refused_reads={} refused_writes={}",
        regs.reads(),
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

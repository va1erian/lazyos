//! Seeded fuzz of the `dev_*` syscall's argument space (issue #497).
//!
//! Every op, including unknown ones, is called with arguments drawn half from
//! the values a correct driver passes and half from edges and noise: handles
//! of the wrong task, kind or generation; BAR indices, offsets, widths and
//! packed `pio` words around every bound; DMA lengths and flags past the pool;
//! pointers that are unmapped, kernel-side or wrapping (checked with user
//! pointer validation on). Three drivers share the devices of
//! [`super::fuzz_world`], and between calls the fuzz fires interrupts, closes
//! buffers and kills drivers.
//!
//! After each call the result must be a value or a known errno, a handle that
//! is not the caller's own live claim must be refused with exactly `EBADF`,
//! the hazards of each device must never be reachable, and the kernel's tables
//! must match the model. At the end, with every driver dead, nothing may be
//! left. A failure names its seed; build with `LAZYOS_DEV_FUZZ_SEED=<n>` to
//! replay just that seed.

use super::fixture::*;
use super::fuzz_world::{Rng, Role, World, EDGES};
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::*;

/// Calls per seed.
const CALLS: u32 = 6_000;
const SEEDS: [u64; 4] = [
    0x9E37_79B9_7F4A_7C15,
    0xD1B5_4A32_D192_ED03,
    0x2545_F491_4F6C_DD1D,
    0x0DEC_AF00_0497_0001,
];

/// Every errno the device syscall may answer with.
const ERRNOS: &[i64] = &[
    EPERM, ENOENT, EBADF, ENOMEM, EACCES, EFAULT, EBUSY, ENODEV, EINVAL, EMFILE, ENOSYS, EDQUOT,
];

/// Ops that take a `Device` handle in `a1`.
const HANDLE_OPS: &[u64] = &[
    OP_MAP_BAR,
    OP_PIO,
    OP_CFG_READ,
    OP_CFG_WRITE,
    OP_IRQ_ENABLE,
    OP_IRQ_ACK,
    OP_RELEASE,
    OP_DMA_ALLOC,
];

/// Rows any table op may be asked to fill (the kernel's device table).
const ROWS: usize = crate::dev::MAX_DEVICES;

/// Counts of what the run reached, so a fuzz that never got past the first
/// check cannot pass.
#[derive(Default)]
struct Reached {
    claims: u32,
    resource_ops: u32,
    dma: u32,
    refused: u32,
}

/// One call's arguments, and the buffer a trusted pointer points into.
struct Call {
    op: u64,
    args: [u64; 4],
    /// Run with kernel pointers trusted (they point into `scratch`).
    trusted: bool,
    scratch: Vec<u64>,
}

fn edge_or_noise(rng: &mut Rng) -> u64 {
    if rng.chance(70) {
        rng.pick(EDGES)
    } else {
        rng.next()
    }
}

/// A handle argument: the actor's own claim, one of its other objects,
/// another actor's number, or noise.
fn handle_arg(world: &World, actor: usize, rng: &mut Rng) -> u64 {
    let me = &world.actors[actor];
    match rng.below(10) {
        0..=4 if !me.claims.is_empty() => me.claims[rng.below(me.claims.len())].0,
        5 if !me.buffers.is_empty() => me.buffers[rng.below(me.buffers.len())].0,
        6 => me.endpoints[rng.below(me.endpoints.len())],
        7 => {
            let other = &world.actors[rng.below(world.actors.len())];
            other.claims.first().map_or(3, |claim| claim.0)
        }
        _ => edge_or_noise(rng),
    }
}

/// A device id: one of ours, or one that names nothing. The machine's own
/// devices are never named (claiming one would quiesce real hardware).
fn device_arg(world: &World, rng: &mut Rng) -> u64 {
    if rng.chance(75) {
        return u64::from(world.ids[rng.below(world.ids.len())].0);
    }
    let value = edge_or_noise(rng);
    let named_real = value < world.real as u64;
    let ours = world.device_of(value).is_some();
    if named_real && !ours {
        crate::dev::MAX_DEVICES as u64 + value
    } else {
        value
    }
}

/// A destination pointer with `words` u64s behind it when trusted.
fn pointer_arg(call: &mut Call, words: usize, rng: &mut Rng) -> u64 {
    if rng.chance(40) {
        call.trusted = true;
        call.scratch = vec![0u64; words.max(1)];
        call.scratch.as_mut_ptr() as u64
    } else {
        // Validation is on: none of these is a writable user range.
        rng.pick(&[0, 8, 0x1000, u64::MAX - 7, 1 << 47, 0xFFFF_8000_0000_0000])
            .wrapping_add(if rng.chance(20) {
                rng.next() & 0xFFF
            } else {
                0
            })
    }
}

fn build_call(world: &World, actor: usize, rng: &mut Rng) -> Call {
    let mut call = Call {
        op: 0,
        args: [0; 4],
        trusted: false,
        scratch: Vec::new(),
    };
    call.op = match rng.below(20) {
        0 => edge_or_noise(rng),
        1 => 13 + rng.below(8) as u64,
        _ => rng.below(13) as u64,
    };
    let me = &world.actors[actor];
    match call.op {
        OP_LIST | OP_INVENTORY | OP_POLICY | OP_DENIALS => {
            let row = if call.op == OP_LIST { ROW_WORDS } else { 4 };
            let capacity = if rng.chance(70) {
                rng.below(ROWS + 1)
            } else {
                edge_or_noise(rng) as usize
            };
            // A trusted pointer must hold whatever the capacity admits.
            let words = capacity.min(ROWS) * row;
            call.args[0] = pointer_arg(&mut call, words, rng);
            call.args[1] = if call.trusted {
                capacity.min(ROWS) as u64
            } else {
                capacity as u64
            };
        }
        OP_CLAIM => {
            call.args[0] = device_arg(world, rng);
            call.args[1] = match rng.below(6) {
                0..=2 => NO_ENDPOINT,
                3 => KERNEL_CHANNEL,
                4 => me.endpoints[rng.below(me.endpoints.len())],
                _ => edge_or_noise(rng),
            };
            call.args[3] = pointer_arg(&mut call, 1, rng);
            call.args[2] = if rng.chance(70) {
                rng.below(2) as u64
            } else {
                edge_or_noise(rng)
            };
        }
        OP_MAP_BAR => {
            call.args[0] = handle_arg(world, actor, rng);
            call.args[1] = if rng.chance(70) {
                rng.below(7) as u64
            } else {
                edge_or_noise(rng)
            };
        }
        OP_PIO => {
            call.args[0] = handle_arg(world, actor, rng);
            call.args[1] = if rng.chance(70) {
                rng.below(7) as u64
            } else {
                edge_or_noise(rng)
            };
            call.args[2] = edge_or_noise(rng) & if rng.chance(60) { 0xF } else { u64::MAX };
            let width = rng.pick(&[0u64, 1, 2, 3, 4, 8, 0xFF]);
            let mut word = pio_word(width, rng.chance(30), rng.next() as u32);
            if rng.chance(15) {
                word |= (rng.next() & 0x7F_FFFF) << 9; // reserved bits
            }
            call.args[3] = word;
        }
        OP_CFG_READ | OP_CFG_WRITE => {
            call.args[0] = handle_arg(world, actor, rng);
            call.args[1] = if rng.chance(50) {
                4
            } else {
                edge_or_noise(rng)
            };
            call.args[2] = rng.pick(&[0u64, 1, 2, 3, 4, 8, u64::MAX]);
            call.args[3] = edge_or_noise(rng);
        }
        OP_IRQ_ENABLE | OP_IRQ_ACK | OP_RELEASE => {
            call.args[0] = handle_arg(world, actor, rng);
            call.args[1] = if rng.chance(80) {
                0
            } else {
                edge_or_noise(rng)
            };
        }
        OP_DMA_ALLOC => {
            call.args[0] = handle_arg(world, actor, rng);
            call.args[1] = if rng.chance(60) {
                rng.pick(&[1u64, 4096, 4097, 8192, 65536])
            } else {
                edge_or_noise(rng)
            };
            call.args[2] = if rng.chance(70) {
                rng.below(4) as u64
            } else {
                edge_or_noise(rng)
            };
            call.args[3] = pointer_arg(&mut call, 1, rng);
        }
        _ => {
            for arg in call.args.iter_mut() {
                *arg = edge_or_noise(rng);
            }
        }
    }
    call
}

/// Run `call` as the current task, with pointer validation unless trusted.
fn run(call: &Call) -> i64 {
    let _strict = (!call.trusted).then(Strict::on);
    let [a1, a2, a3, a4] = call.args;
    sys(call.op, a1, a2, a3, a4)
}

/// Judge one result against the model and update the model.
fn judge(
    world: &mut World,
    actor: usize,
    call: &Call,
    got: i64,
    reached: &mut Reached,
) -> Result<(), String> {
    let what = || format!("op {:#x} args {:x?} by actor {actor}", call.op, call.args);
    check!(
        got >= 0 || ERRNOS.contains(&-got),
        "{}: unexpected result {got}",
        what()
    );
    if got < 0 {
        reached.refused += 1;
    }
    if HANDLE_OPS.contains(&call.op) {
        let Some(device) = world.actors[actor].claim_of(call.args[0]) else {
            check!(
                got == -EBADF,
                "{}: a foreign handle got {got}, not EBADF",
                what()
            );
            return Ok(());
        };
        check!(
            got != -EBADF,
            "{}: the caller's own live claim got EBADF",
            what()
        );
        if got < 0 {
            return Ok(());
        }
        let role = world.role(device);
        check!(
            !(role == Role::Hostile && matches!(call.op, OP_MAP_BAR | OP_PIO)),
            "{}: a hostile BAR was reachable",
            what()
        );
        check!(
            !(role == Role::Bridge && call.op == OP_DMA_ALLOC),
            "{}: a bridge got DMA memory",
            what()
        );
        check!(
            !(role == Role::Platform && matches!(call.op, OP_CFG_READ | OP_CFG_WRITE)),
            "{}: a platform device has no config space",
            what()
        );
        reached.resource_ops += 1;
        match call.op {
            OP_RELEASE => world.end_claim(actor, device),
            OP_DMA_ALLOC => {
                reached.dma += 1;
                world.actors[actor].buffers.push((got as u64, device));
            }
            _ => {}
        }
        return Ok(());
    }
    if call.op == OP_CLAIM && got >= 0 {
        let device = world
            .device_of(call.args[0])
            .ok_or_else(|| format!("{}: claimed a device the fuzz never named", what()))?;
        check!(
            world.actors[actor].cap,
            "{}: claimed without CAP_DEV_CLAIM",
            what()
        );
        check!(
            world.owner[device].is_none(),
            "{}: claimed a device {:?} already holds",
            what(),
            world.owner[device]
        );
        world.owner[device] = Some(actor);
        world.actors[actor].claims.push((got as u64, device));
        reached.claims += 1;
    }
    Ok(())
}

/// Something other than a call: an interrupt, a closed buffer, a dead driver.
fn perturb(world: &mut World, fx: &Fixture, rng: &mut Rng) -> Result<(), String> {
    world.now += 1;
    match rng.below(4) {
        0 => {
            leave(fx);
            fire(rng.pick(&[LINE_A, LINE_B, LINE_C]), world.now);
        }
        1 => {
            let actor = rng.below(world.actors.len());
            let held = world.actors[actor].buffers.len();
            if held > 0 {
                let (buffer, _) = world.actors[actor].buffers.remove(rng.below(held));
                enter(world.actors[actor].slot)?;
                crate::ipc::shared::close(buffer).map_err(|e| e.message().to_string())?;
            }
        }
        2 if rng.chance(25) => {
            let actor = rng.below(world.actors.len());
            world.respawn(fx, actor)?;
        }
        _ => {
            leave(fx);
            crate::dev::intx::service_at(world.now);
        }
    }
    Ok(())
}

fn fuzz_seed(seed: u64) -> Result<Reached, String> {
    let fx = Fixture::new()?;
    let before = idle_pool()?;
    let mut world = World::new()?;
    let mut rng = Rng::new(seed);
    let mut reached = Reached::default();
    for step in 0..CALLS {
        if rng.chance(8) {
            perturb(&mut world, &fx, &mut rng)
                .map_err(|e| format!("seed {seed:#x} step {step}: {e}"))?;
        }
        let actor = rng.below(world.actors.len());
        enter(world.actors[actor].slot)?;
        let call = build_call(&world, actor, &mut rng);
        let got = run(&call);
        judge(&mut world, actor, &call, got, &mut reached)
            .and_then(|()| world.verify("after the call"))
            .map_err(|e| format!("seed {seed:#x} step {step}: {e}"))?;
    }
    world
        .finish(&fx, before)
        .map_err(|e| format!("seed {seed:#x} at the end: {e}"))?;
    Ok(reached)
}

/// The fuzz itself: every seed, then a floor on what was reached.
pub fn dev_fuzz_syscall_arguments() -> Result<(), String> {
    let replay = option_env!("LAZYOS_DEV_FUZZ_SEED").and_then(|text| {
        let text = text.trim();
        match text.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => text.parse().ok(),
        }
    });
    let seeds: Vec<u64> = replay.map_or_else(|| SEEDS.to_vec(), |seed| vec![seed]);
    let mut total = Reached::default();
    for seed in &seeds {
        let reached = fuzz_seed(*seed)?;
        total.claims += reached.claims;
        total.resource_ops += reached.resource_ops;
        total.dma += reached.dma;
        total.refused += reached.refused;
    }
    let calls = CALLS * seeds.len() as u32;
    check!(
        total.claims >= 50 && total.resource_ops >= 200 && total.dma >= 10 && total.refused >= calls / 4,
        "the fuzz reached too little: {} claims, {} resource ops, {} DMA buffers, {} refusals in {calls} calls",
        total.claims,
        total.resource_ops,
        total.dma,
        total.refused
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] =
    &[("dev_fuzz_syscall_arguments", dev_fuzz_syscall_arguments)];

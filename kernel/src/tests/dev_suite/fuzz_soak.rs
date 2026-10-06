//! A randomized lifecycle soak of the device syscall (issue #497).
//!
//! Where the argument fuzz ([`super::fuzz`]) throws hostile values at every
//! op, this soak drives *valid* sequences for a long time: three drivers claim
//! the devices of [`super::fuzz_world`] with and without interrupt endpoints,
//! arm their lines, map BARs, do port I/O, allocate and close DMA buffers,
//! take and acknowledge interrupts, release, and crash, in a seeded random
//! order. The model is checked every step; at the end every driver is killed
//! and frames, quotas, the DMA pool and the PIC must be back where they were.

use super::fixture::*;
use super::fuzz_world::{Role, Rng, World};
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::*;

const STEPS: u32 = 30_000;
const SEED: u64 = 0x5EED_0497_D0C5_0A4B;

/// One step: pick an actor and something sensible for it to do.
fn step(world: &mut World, fx: &Fixture, rng: &mut Rng, served: &mut u32) -> Result<(), String> {
    world.now += 1;
    let actor = rng.below(2); // the two capable drivers
    let slot = world.actors[actor].slot;
    enter(slot)?;
    let claims = world.actors[actor].claims.clone();
    match rng.below(10) {
        // Claim a free device, sometimes with an interrupt endpoint.
        0 | 1 => {
            let device = rng.below(world.ids.len());
            // The bridge has no interrupt line to listen on.
            let listen = rng.chance(50) && world.role(device) != Role::Bridge;
            let channel = if listen { Some(irq_channel()?) } else { None };
            let endpoint = channel.map_or(NO_ENDPOINT, |(endpoint, _)| endpoint);
            let got = claim_irq(world.ids[device], endpoint, listen && rng.chance(50));
            match world.owner[device] {
                None => {
                    let handle = expect_ok(got, "claim of a free device")?;
                    world.owner[device] = Some(actor);
                    world.actors[actor].claims.push((handle, device));
                    if listen {
                        world.actors[actor].irq.push((handle, endpoint));
                    }
                }
                Some(_) => {
                    expect_errno(got, EBUSY, "claim of an owned device")?;
                    if let Some((endpoint, peer)) = channel {
                        let _ = channels::close_endpoint(endpoint);
                        let _ = channels::close_endpoint(peer);
                    }
                }
            }
        }
        // Use a held claim the way a driver does.
        2..=5 if !claims.is_empty() => {
            let (handle, device) = claims[rng.below(claims.len())];
            match (world.role(device), rng.below(5)) {
                (Role::Nic, 0) => {
                    let got = sys(OP_MAP_BAR, handle, 0, 0, 0);
                    // A second map of the same BAR is EBUSY.
                    check!(got >= 0 || got == -EBUSY, "map_bar: {got}");
                }
                (Role::Nic | Role::Platform, 1) => {
                    expect_ok(sys(OP_PIO, handle, 1, 4, pio_word(4, false, 0)), "pio")?;
                }
                (Role::Nic | Role::Bridge, 2) => {
                    expect_ok(sys(OP_CFG_READ, handle, 0, 4, 0), "cfg_read")?;
                    // Bus mastering needs the DMA right a bridge never has.
                    let want = if world.role(device) == Role::Bridge { -EPERM } else { 0 };
                    check!(sys(OP_CFG_WRITE, handle, 4, 2, 0x0006) == want, "cfg_write");
                    expect_ok(sys(OP_CFG_WRITE, handle, 4, 2, 0x0002), "cfg_write decode only")?;
                }
                (Role::Nic | Role::Hostile | Role::Platform, 3) => {
                    let mut bus = 0;
                    let got = dma_alloc(handle, 4096 * (1 + rng.below(4) as u64), 0, &mut bus);
                    // Neither a bridge nor a platform device masters the bus.
                    if world.role(device) == Role::Platform {
                        expect_errno(got, EPERM, "dma on a platform device")?;
                    } else if got >= 0 {
                        world.actors[actor].buffers.push((got as u64, device));
                    } else {
                        // The per-claim record table or the quota is full.
                        check!(got == -EMFILE || got == -EDQUOT, "dma_alloc: {got}");
                    }
                }
                _ => {
                    // Arm or acknowledge; both may legitimately refuse (no
                    // endpoint, unroutable line, nothing to acknowledge).
                    let op = if rng.chance(50) { OP_IRQ_ENABLE } else { OP_IRQ_ACK };
                    let got = sys(op, handle, 0, 0, 0);
                    check!(got >= 0 || [EPERM, EINVAL, ENOSYS, ENOENT].contains(&-got), "irq op {op}: {got}");
                }
            }
        }
        // Close a buffer.
        6 if !world.actors[actor].buffers.is_empty() => {
            let held = world.actors[actor].buffers.len();
            let (buffer, _) = world.actors[actor].buffers.remove(rng.below(held));
            crate::ipc::shared::close(buffer).map_err(|e| e.message().to_string())?;
        }
        // Release.
        7 if !claims.is_empty() => {
            let (handle, device) = claims[rng.below(claims.len())];
            expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
            world.end_claim(actor, device);
        }
        // An interrupt arrives; whoever got a message services and acks it.
        8 => {
            leave(fx);
            fire(rng.pick(&[LINE_A, LINE_B, LINE_C]), world.now);
            *served += drain_interrupts(world)?;
        }
        // A driver crashes.
        9 if rng.chance(20) => world.respawn(fx, actor)?,
        _ => {}
    }
    Ok(())
}

/// Every listening claim takes the interrupt messages it was sent, checks
/// they came from the kernel, and acknowledges each one.
fn drain_interrupts(world: &World) -> Result<u32, String> {
    let mut served = 0;
    for actor in &world.actors[..2] {
        enter(actor.slot)?;
        for &(handle, endpoint) in &actor.irq {
            while queued(endpoint)? > 0 {
                let (sender, ..) = take_irq(endpoint)?;
                check!(sender == task::KERNEL_TASK, "an interrupt from task {sender}");
                expect_ok(sys(OP_IRQ_ACK, handle, 0, 0, 0), "ack of a delivered interrupt")?;
                served += 1;
            }
        }
    }
    Ok(served)
}

/// One seeded run over a fresh world, ending with every driver dead and
/// nothing left over; the number of interrupts serviced.
fn soak_once(fx: &Fixture) -> Result<u32, String> {
    let before = idle_pool()?;
    let mut world = World::new()?;
    let mut rng = Rng::new(SEED);
    let mut served = 0;
    for index in 0..STEPS {
        step(&mut world, fx, &mut rng, &mut served)
            .and_then(|()| world.verify("after the step"))
            .map_err(|e| format!("soak step {index}: {e}"))?;
    }
    check!(served > 0, "no interrupt was ever acknowledged in {STEPS} steps");
    world.finish(fx, before)?;
    Ok(served)
}

/// The soak, twice with the same seed. The first run warms every pool the
/// kernel grows on demand (heap, channel and handle tables); the second makes
/// exactly the same calls, so any frame it does not give back is a leak.
pub fn dev_fuzz_lifecycle_soak() -> Result<(), String> {
    let fx = Fixture::new()?;
    let first = soak_once(&fx)?;
    let warm = mem::frame_stats();
    let second = soak_once(&fx)?;
    let after = mem::frame_stats();
    check!(first == second, "the same seed serviced {first} then {second} interrupts");
    check!(
        after.live() == warm.live() && after.double_frees == warm.double_frees,
        "frames {} -> {} over an identical second run (double frees {} -> {})",
        warm.live(),
        after.live(),
        warm.double_frees,
        after.double_frees
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[("dev_fuzz_lifecycle_soak", dev_fuzz_lifecycle_soak)];

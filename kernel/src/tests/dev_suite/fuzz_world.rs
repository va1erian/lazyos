//! The world the `dev_*` fuzz runs in (issue #497): a seeded generator, four
//! synthetic devices with distinct hazards, three driver tasks, and a model of
//! who owns what that every call is judged against.
//!
//! The model is deliberately tiny: device owner and generation, each actor's
//! live claim handles and DMA buffers. That is enough to state the properties
//! that matter: only a task's own live claim handle ever acts on a device, a
//! claim needs `CAP_DEV_CLAIM` and a free device, ownership and generations in
//! the kernel's tables match the model after every call, and the claim quota
//! equals the number of live claims.

use super::fixture::*;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::quota::Resource;

/// The HPET window: present on both QEMU machine types, never RAM.
pub const DEVICE_MEM: u64 = 0xFED0_0000;

/// A deterministic generator (xorshift64*), so a printed seed replays.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }

    /// True `percent` times in a hundred.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// Values on the edges the syscall checks (widths, alignment, page and
/// 32-bit boundaries, sign bits).
pub const EDGES: &[u64] = &[
    0,
    1,
    2,
    3,
    4,
    5,
    6,
    7,
    8,
    0xF,
    0x10,
    0x3C,
    0x40,
    0xFF,
    0x100,
    0xFFF,
    0x1000,
    0x1001,
    0x2000,
    0xFFFF,
    0x1_0000,
    0x7FFF_FFFF,
    0xFFFF_FFFF,
    1 << 32,
    1 << 63,
    u64::MAX - 1,
    u64::MAX,
];

/// What a device is in the fuzz: each carries a hazard a broken check would
/// let through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// A normal NIC: memory BAR, I/O BAR, interrupt line.
    Nic,
    /// I/O BARs over the PIC (0x60) and the PCI config ports (0xCF8), and
    /// memory BARs that are unassigned or unaligned: no `map_bar` or `pio`
    /// may ever succeed on it.
    Hostile,
    /// A PCI bridge: never `DMA`.
    Bridge,
    /// A platform device: no configuration space.
    Platform,
}

pub const ROLES: [Role; 4] = [Role::Nic, Role::Hostile, Role::Bridge, Role::Platform];

fn spec(role: Role) -> Spec {
    match role {
        Role::Nic => Spec::nic(Some(LINE_A))
            .with_bars(vec![mem_bar(0, DEVICE_MEM, 0x2000), io_bar(1, 0x0700, 8)]),
        Role::Hostile => Spec::nic(Some(LINE_B)).with_bars(vec![
            io_bar(0, 0x60, 8),
            io_bar(1, 0xCF8, 8),
            mem_bar(2, 0, 0x1000),
            mem_bar(3, DEVICE_MEM + 0x100, 0x1000),
            io_bar(4, 0xFFFC, 8),
        ]),
        Role::Bridge => Spec::nic(None).with_class(0x06, 0x04),
        Role::Platform => Spec::nic(Some(LINE_C)).platform(),
    }
}

/// One driver task and what the model says it holds.
pub struct Actor {
    pub slot: usize,
    pub cap: bool,
    /// Live claim handles and the device each names.
    pub claims: Vec<(u64, usize)>,
    /// Live DMA buffers and the device they were allocated through.
    pub buffers: Vec<(u64, usize)>,
    /// Channel endpoints a claim may name for interrupts.
    pub endpoints: Vec<u64>,
    /// Claims made with an interrupt endpoint: `(claim handle, endpoint)`.
    pub irq: Vec<(u64, u64)>,
}

impl Actor {
    pub fn claim_of(&self, handle: u64) -> Option<usize> {
        self.claims
            .iter()
            .find(|(held, _)| *held == handle)
            .map(|(_, device)| *device)
    }
}

/// The devices, the actors and the model.
pub struct World {
    pub ids: [DeviceId; 4],
    pub owner: [Option<usize>; 4],
    pub generation: [u32; 4],
    pub actors: Vec<Actor>,
    /// Device ids below this are the machine's own: never claimed by the fuzz.
    pub real: usize,
    pub now: u64,
}

impl World {
    /// Add the devices and spawn two capable drivers and one without the
    /// capability (all the same uid, so quotas are shared as they would be).
    pub fn new() -> Result<World, String> {
        let real = crate::dev::table().lock().len();
        let mut ids = [DeviceId(0); 4];
        let mut generation = [0; 4];
        for (index, role) in ROLES.into_iter().enumerate() {
            ids[index] = add_device(spec(role))?;
            generation[index] = table_state(ids[index]).1;
        }
        let mut world = World {
            ids,
            owner: [None; 4],
            generation,
            actors: Vec::new(),
            real,
            now: 1_000,
        };
        for cap in [true, true, false] {
            let actor = world.spawn(cap)?;
            world.actors.push(actor);
        }
        Ok(world)
    }

    fn spawn(&self, cap: bool) -> Result<Actor, String> {
        let mut cred = driver_cred();
        if !cap {
            cred.caps = 0;
        }
        let slot = spawn_driver(cred)?;
        enter(slot)?;
        let mut endpoints = Vec::new();
        for _ in 0..3 {
            endpoints.push(irq_channel()?.0);
        }
        task::harness::switch_current(task::KERNEL_TASK);
        Ok(Actor {
            slot,
            cap,
            claims: Vec::new(),
            buffers: Vec::new(),
            endpoints,
            irq: Vec::new(),
        })
    }

    pub fn role(&self, device: usize) -> Role {
        ROLES[device]
    }

    /// The model index of a device id, if it is one of ours.
    pub fn device_of(&self, id: u64) -> Option<usize> {
        self.ids.iter().position(|dev| u64::from(dev.0) == id)
    }

    /// A claim handle ended (release or teardown): the device is free at the
    /// next generation and the buffers allocated through it are closed.
    pub fn end_claim(&mut self, actor: usize, device: usize) {
        let ended: Vec<u64> = self.actors[actor]
            .claims
            .iter()
            .filter(|(_, held)| *held == device)
            .map(|(handle, _)| *handle)
            .collect();
        self.actors[actor]
            .irq
            .retain(|(claim, _)| !ended.contains(claim));
        self.actors[actor]
            .claims
            .retain(|(_, held)| *held != device);
        self.actors[actor]
            .buffers
            .retain(|(_, through)| *through != device);
        self.owner[device] = None;
        self.generation[device] = self.generation[device].wrapping_add(1);
    }

    /// Kill actor `index`'s task and give it a fresh one: every claim ends.
    pub fn respawn(&mut self, fixture: &Fixture, index: usize) -> Result<(), String> {
        leave(fixture);
        let slot = self.actors[index].slot;
        task::harness::finish(slot, 0);
        check!(
            task::reap_child_slot(slot).is_some(),
            "the killed driver {slot} was not reaped"
        );
        let devices: Vec<usize> = self.actors[index].claims.iter().map(|c| c.1).collect();
        for device in devices {
            self.end_claim(index, device);
        }
        let cap = self.actors[index].cap;
        self.actors[index] = self.spawn(cap)?;
        Ok(())
    }

    /// The kernel's tables agree with the model.
    pub fn verify(&self, step: &str) -> Result<(), String> {
        let mut live = 0;
        for device in 0..self.ids.len() {
            let (owner, generation) = table_state(self.ids[device]);
            let expected =
                self.owner[device].map(|actor| crate::dev::TaskSlot(self.actors[actor].slot));
            check!(
                owner == expected && generation == self.generation[device],
                "{step}: device {device:?} ({:?}) is owner {owner:?} gen {generation}, model {expected:?} gen {}",
                self.role(device),
                self.generation[device]
            );
            live += usize::from(owner.is_some());
        }
        check!(
            CLAIMS.lock().len() == live,
            "{step}: {} claim records for {live} owned devices",
            CLAIMS.lock().len()
        );
        check!(
            usage(Resource::DeviceClaims) == live as u64,
            "{step}: DeviceClaims usage {} for {live} claims",
            usage(Resource::DeviceClaims)
        );
        Ok(())
    }

    /// Kill every actor and require that nothing is left: claims, quota,
    /// DMA pool pages, masked lines.
    pub fn finish(
        mut self,
        fixture: &Fixture,
        before: crate::mem::dma::DmaStats,
    ) -> Result<(), String> {
        for index in 0..self.actors.len() {
            leave(fixture);
            let slot = self.actors[index].slot;
            task::harness::finish(slot, 0);
            check!(
                task::reap_child_slot(slot).is_some(),
                "driver {slot} was not reaped"
            );
            let devices: Vec<usize> = self.actors[index].claims.iter().map(|c| c.1).collect();
            for device in devices {
                self.end_claim(index, device);
            }
        }
        check!(CLAIMS.lock().len() == 0, "a claim outlived its task");
        for resource in [
            Resource::DeviceClaims,
            Resource::UserMemory,
            Resource::DmaMemory,
            Resource::Handles,
        ] {
            check!(
                usage(resource) == 0,
                "{resource:?} usage {} after every driver died",
                usage(resource)
            );
        }
        let after = pool();
        check!(
            after.free_pages == before.free_pages,
            "the DMA pool lost pages: {} -> {}",
            before.free_pages,
            after.free_pages
        );
        for line in [LINE_A, LINE_B, LINE_C] {
            check!(masked(line), "line {line} left unmasked");
        }
        Ok(())
    }
}

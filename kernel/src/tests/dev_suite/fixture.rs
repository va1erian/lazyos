//! Shared scaffolding for the device-syscall and interrupt suites (issue #240).
//!
//! The suites share one process and one global device table, IRQ state, ACL
//! policy, quota ledger and audit ring, so every test builds a [`Fixture`],
//! whose `Drop` puts all of that back on *every* exit path, including a failed
//! `check!`. Synthetic devices are appended after the real ones and truncated
//! away again; the real devices are never claimed or reprogrammed (the one
//! test that touches a real function saves and restores it).

use super::*;
// Re-exported so every device test module gets one consistent vocabulary from
// `use super::fixture::*`.
pub use crate::arch::pic;
pub use crate::dev::syscall::NO_ENDPOINT;
pub use crate::dev::{intx, irq, pci, Bar, BarKind, BusId, DeviceId, DeviceInfo, Resources};
pub use crate::ipc::credentials::{Cred, CAP_DEV_CLAIM};
pub use crate::ipc::{acl, audit, channels, handles};
pub use alloc::string::ToString;
pub use x86_64::PhysAddr;

use crate::dev::syscall as devsys;
use crate::quota;
use libmessenger::{flags, Decoder, Encoder, Header, Parcel, VERSION};

/// The uid every test driver runs as.
pub const DRIVER_UID: u32 = 4200;
/// Interrupt lines the suite owns: unassigned on the machine types we boot.
pub const LINE_A: u8 = 9;
pub const LINE_B: u8 = 10;
pub const LINE_C: u8 = 11;

/// A PCI address no machine populates: config reads return all-ones and writes
/// are ignored, so claim/quiesce/cfg on synthetic devices never touch hardware.
pub const GHOST: pci::Address = pci::Address {
    bus: 0xFE,
    device: 0,
    function: 0,
};

/// A driver's credentials: unprivileged apart from `CAP_DEV_CLAIM`.
pub fn driver_cred() -> Cred {
    Cred::new(DRIVER_UID, DRIVER_UID, CAP_DEV_CLAIM, 0, 1)
}

/// Encode `Bar` shorthands.
pub fn mem_bar(index: u8, base: u64, len: u64) -> Bar {
    Bar {
        index,
        kind: BarKind::Mem,
        base,
        len,
        is_64: false,
        prefetchable: false,
    }
}

pub fn io_bar(index: u8, base: u64, len: u64) -> Bar {
    Bar {
        index,
        kind: BarKind::Io,
        base,
        len,
        is_64: false,
        prefetchable: false,
    }
}

/// What a synthetic device looks like.
pub struct Spec {
    pub class: u8,
    pub subclass: u8,
    pub pci: bool,
    pub bars: Vec<Bar>,
    pub irq: Option<u8>,
}

impl Spec {
    /// A PCI network card with one memory BAR and an interrupt line.
    pub fn nic(irq: Option<u8>) -> Spec {
        Spec {
            class: 0x02,
            subclass: 0,
            pci: true,
            bars: vec![mem_bar(0, 0xFED0_0000, 0x1000), io_bar(1, 0x0700, 8)],
            irq,
        }
    }

    pub fn with_class(mut self, class: u8, subclass: u8) -> Spec {
        self.class = class;
        self.subclass = subclass;
        self
    }

    pub fn with_bars(mut self, bars: Vec<Bar>) -> Spec {
        self.bars = bars;
        self
    }

    pub fn platform(mut self) -> Spec {
        self.pci = false;
        self
    }
}

/// Append a synthetic device to the global table.
pub fn add_device(spec: Spec) -> Result<DeviceId, String> {
    let mut resources = Resources::empty();
    for bar in spec.bars {
        resources.set_bar(bar);
    }
    if let Some(line) = spec.irq {
        resources.set_irq(crate::dev::Irq { line });
    }
    let info = DeviceInfo {
        id: DeviceId(0),
        bus: if spec.pci {
            BusId::Pci(GHOST)
        } else {
            BusId::Platform
        },
        vendor: 0x1AF4,
        device: 0x1000,
        subsystem_vendor: 0x1AF4,
        subsystem_device: 1,
        class: spec.class,
        subclass: spec.subclass,
        prog_if: 0,
        revision: 0,
        resources,
    };
    crate::dev::table()
        .lock()
        .insert(info)
        .map_err(|error| format!("cannot add a synthetic device: {error:?}"))
}

/// Restores every piece of global state a device test may change.
pub struct Fixture {
    base_len: usize,
    kernel_table: PhysAddr,
    masks: [bool; 3],
}

impl Fixture {
    pub fn new() -> Result<Fixture, String> {
        crate::dev::init();
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        channels::reset();
        quota::reset();
        for slot in 0..task::MAX_TASKS {
            handles::reset_for_task(slot);
            crate::ipc::credentials::reset_for_task(slot);
        }
        acl::load(&[]);
        audit::reset();
        intx::reset_for_test();
        // Room for the many claims one test makes; individual tests lower it.
        quota::set_limit(DRIVER_UID, quota::Resource::DeviceClaims, 64);
        let base_len = crate::dev::table().lock().len();
        let masks = [LINE_A, LINE_B, LINE_C].map(pic::is_masked);
        check!(
            masks.iter().all(|masked| *masked),
            "test lines are not masked at the PIC to begin with: {masks:?}"
        );
        Ok(Fixture {
            base_len,
            kernel_table: mem::kernel_table(),
            masks,
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        mem::switch_to(self.kernel_table);
        task::harness::switch_current(task::KERNEL_TASK);
        for slot in 1..task::MAX_TASKS {
            crate::dev::teardown_task(slot, 0);
        }
        for line in [LINE_A, LINE_B, LINE_C] {
            irq::unregister_kernel(line);
        }
        let _ = irq::take_raised();
        intx::reset_for_test();
        crate::dev::table().lock().truncate_for_test(self.base_len);
        acl::load(&[]);
        quota::reset();
        channels::reset();
        for (line, was_masked) in [LINE_A, LINE_B, LINE_C].into_iter().zip(self.masks) {
            pic::set_masked(line, was_masked);
        }
    }
}

/// Spawn a driver task with `cred`, leaving the kernel task current.
pub fn spawn_driver(cred: Cred) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(to_string)?;
    handles::reset_for_task(slot);
    crate::ipc::credentials::set(slot, cred);
    Ok(slot)
}

/// Make `slot` the current task, running in its own address space.
pub fn enter(slot: usize) -> Result<(), String> {
    let table = task::harness::pml4(slot).ok_or("the task has no address space")?;
    task::harness::switch_current(slot);
    mem::switch_to(PhysAddr::new(table));
    Ok(())
}

/// Back to the kernel task and its page tables.
pub fn leave(fixture: &Fixture) {
    mem::switch_to(fixture.kernel_table);
    task::harness::switch_current(task::KERNEL_TASK);
}

/// One `dev_*` syscall as the current task; the value or `-errno`.
pub fn sys(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    devsys::dispatch(op, a1, a2, a3, a4) as i64
}

/// Require `got == -errno`.
pub fn expect_errno(got: i64, errno: i64, what: &str) -> Result<(), String> {
    check!(got == -errno, "{what}: got {got}, expected -{errno}");
    Ok(())
}

/// Require a successful (non-negative) result and return it.
pub fn expect_ok(got: i64, what: &str) -> Result<u64, String> {
    check!(got >= 0, "{what}: failed with {got}");
    Ok(got as u64)
}

/// `claim` with no interrupt endpoint.
pub fn claim_plain(id: DeviceId) -> i64 {
    sys(devsys::OP_CLAIM, u64::from(id.0), NO_ENDPOINT, 0, 0)
}

/// A driver's interrupt channel in the current task: the endpoint handle it
/// names at `claim` and the peer handle a test uses to fill its inbox.
pub fn irq_channel() -> Result<(u64, u64), String> {
    channels::create().map_err(|error| error.message().to_string())
}

/// `claim` with an interrupt endpoint (and optional sharing).
pub fn claim_irq(id: DeviceId, endpoint: u64, shared: bool) -> i64 {
    sys(
        devsys::OP_CLAIM,
        u64::from(id.0),
        endpoint,
        u64::from(shared),
        0,
    )
}

/// Messages waiting in the inbox behind `endpoint` (current task's handle).
pub fn queued(endpoint: u64) -> Result<u64, String> {
    Ok(channels::channel_stats(endpoint)
        .map_err(|error| error.message().to_string())?
        .queued)
}

/// Receive one message and decode it as an interrupt notification:
/// `(sender, device id, irq index, generation)`.
pub fn take_irq(endpoint: u64) -> Result<(usize, u32, u32, u32), String> {
    let message = channels::try_recv(endpoint)
        .map_err(|error| error.message().to_string())?
        .ok_or("no interrupt message was queued")?;
    check!(
        message.method == crate::dev::class::method::IRQ,
        "the message has method {:#x}, not `irq`",
        message.method
    );
    check!(message.txn.is_none(), "an interrupt message is one-way");
    let parcel = Parcel::decode(&message.bytes).map_err(|error| error.message())?;
    check!(
        parcel.header.interface_id == crate::dev::class::DEV_INTERFACE,
        "the message is from interface {:#x}",
        parcel.header.interface_id
    );
    let mut decoder = Decoder::new(&parcel.body);
    let mut fields = [0u32; 3];
    while let Some(field) = decoder.next().map_err(|error| error.message())? {
        if let Some(slot) = fields.get_mut(usize::from(field.id).wrapping_sub(1)) {
            *slot = field.as_u32().map_err(|error| error.message())?;
        }
    }
    Ok((message.sender, fields[0], fields[1], fields[2]))
}

/// Send one one-way message into the inbox behind `endpoint`'s peer, to fill it.
pub fn stuff_inbox(peer: u64) -> Result<(), channels::Error> {
    let mut body = Encoder::new();
    body.u32(1, 1).map_err(|_| channels::Error::BadParcel)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: 0x77,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel
        .encode(&mut bytes)
        .map_err(|_| channels::Error::BadParcel)?;
    channels::send(peer, &bytes)
}

/// Simulate the ISR for `line` followed by the bottom half at tick `now`.
pub fn fire(line: u8, now: u64) {
    irq::dispatch(line);
    intx::service_at(now);
}

/// Whether `line` is masked at the PIC right now.
pub fn masked(line: u8) -> bool {
    pic::is_masked(line)
}

/// Device generation and owner straight from the table.
pub fn table_state(id: DeviceId) -> (Option<crate::dev::TaskSlot>, u32) {
    let table = crate::dev::table().lock();
    (table.owner(id), table.generation(id).unwrap_or(u32::MAX))
}

/// Number of claims the claim table holds.
pub fn claim_count() -> usize {
    let mut owned = 0;
    let table = crate::dev::table().lock();
    for info in table.iter() {
        if table
            .owner(info.id)
            .is_some_and(|slot| slot != crate::dev::TaskSlot::KERNEL)
        {
            owned += 1;
        }
    }
    owned
}

/// The uid's usage of `resource`.
pub fn usage(resource: quota::Resource) -> u64 {
    quota::usage(DRIVER_UID, resource)
}

/// `dma_alloc` (issue #241) with a bus-address output buffer.
pub fn dma_alloc(handle: u64, len: u64, flags: u64, out: &mut u64) -> i64 {
    sys(
        devsys::OP_DMA_ALLOC,
        handle,
        len,
        flags,
        out as *mut u64 as u64,
    )
}

/// The DMA pool's free-space snapshot.
pub fn pool() -> crate::mem::dma::DmaStats {
    crate::mem::dma_stats()
}

/// A pool with no live allocations, for a balanced test to return to.
pub fn idle_pool() -> Result<crate::mem::dma::DmaStats, String> {
    let stats = pool();
    check!(
        stats.free_pages == stats.total_pages,
        "the DMA pool was not idle at test start: {stats:?}"
    );
    Ok(stats)
}

/// Backing frames of a buffer handle, for contiguity and identity checks.
pub fn frames_of(handle: u64) -> Result<Vec<u64>, String> {
    crate::ipc::shared::harness::frames(handle)
        .map(|frames| frames.iter().map(|frame| frame.as_u64()).collect())
        .map_err(|error| error.message().to_string())
}

/// Open a channel in the calling task, then mirror the receiving endpoint into
/// `slot`'s table. Returns the caller's sender handle and the receiver's
/// mirror (both name the same channel).
pub fn channel_to(slot: usize) -> Result<(u64, u64), String> {
    let (client, server) = channels::create().map_err(|error| error.message().to_string())?;
    let entry = handles::get(server).map_err(|error| error.message().to_string())?;
    let caller = task::current();
    task::harness::switch_current(slot);
    let mirror = handles::open(handles::HandleKind::Channel, entry.rights, entry.object_id)
        .map_err(|error| error.message().to_string())?;
    task::harness::switch_current(caller);
    Ok((client, mirror))
}

/// Build and send a one-way parcel that transfers `buffer` to the endpoint's
/// peer (the sender's handle moves; its mapping goes with it).
pub fn send_buffer(endpoint: u64, buffer: u64) -> Result<(), String> {
    let mut body = Encoder::new();
    body.u64(1, 7).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: 0x0bad_cafe,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: vec![buffer],
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    channels::send(endpoint, &bytes).map_err(|error| error.message().to_string())
}

/// Transfer `buffer` from `from` (the current task) to `to`, receive it there,
/// close it, and return to `from`. Used to keep a long-lived client from
/// accumulating handles across a soak.
pub fn transfer_and_consume(from: usize, to: usize, buffer: u64) -> Result<(), String> {
    let (endpoint, server) = channel_to(to)?;
    send_buffer(endpoint, buffer)?;
    task::harness::switch_current(to);
    mem::switch_to(PhysAddr::new(task::harness::pml4(to).ok_or("no table")?));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    check!(
        message.handles.len() == 1,
        "the transfer delivered {} handles",
        message.handles.len()
    );
    crate::ipc::shared::close(message.handles[0]).map_err(|error| error.message().to_string())?;
    channels::close_endpoint(server).map_err(|error| error.message().to_string())?;
    task::harness::switch_current(from);
    mem::switch_to(PhysAddr::new(task::harness::pml4(from).ok_or("no table")?));
    Ok(())
}

/// [`transfer_and_consume`] without the close: returns the handle the client
/// now holds (in the client table), so the buffer outlives its sender.
pub fn transfer_and_hold(from: usize, to: usize, buffer: u64) -> Result<u64, String> {
    let (endpoint, server) = channel_to(to)?;
    send_buffer(endpoint, buffer)?;
    task::harness::switch_current(to);
    mem::switch_to(PhysAddr::new(task::harness::pml4(to).ok_or("no table")?));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    check!(
        message.handles.len() == 1,
        "the transfer delivered no handle"
    );
    let held = message.handles[0];
    channels::close_endpoint(server).map_err(|error| error.message().to_string())?;
    task::harness::switch_current(from);
    mem::switch_to(PhysAddr::new(task::harness::pml4(from).ok_or("no table")?));
    Ok(held)
}

/// The encoded `pio` request word.
pub fn pio_word(width: u64, write: bool, value: u32) -> u64 {
    width | u64::from(write) << 8 | u64::from(value) << 32
}

/// Turns user-pointer validation on for its lifetime: the suite normally
/// passes kernel buffers as "user" pointers, so hostile-pointer tests opt in.
pub struct Strict(bool);

impl Strict {
    pub fn on() -> Strict {
        Strict(crate::user_ptr::set_trust_kernel_pointers(false))
    }
}

impl Drop for Strict {
    fn drop(&mut self) {
        crate::user_ptr::set_trust_kernel_pointers(self.0);
    }
}

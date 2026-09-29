//! Device core (issue #239, driver-plan D1): enumeration, BAR sizing, claim
//! and generation invalidation. Correctness plus the 1M claim/release soak.

use super::*;
use crate::dev::{
    attach_all, pci, Bar, BarKind, BusId, DevError, DeviceHandle, DeviceId, DeviceInfo,
    DeviceTable, Driver, Resources, TaskSlot,
};
use spin::Mutex;

/// A synthetic device for table tests, independent of hardware.
fn test_device(class: u8) -> DeviceInfo {
    let mut resources = Resources::empty();
    resources.set_bar(Bar {
        index: 0,
        kind: BarKind::Mem,
        base: 0x1000,
        len: 0x1000,
        is_64: false,
        prefetchable: false,
    });
    DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Platform,
        vendor: 0x1234,
        device: 0x5678,
        subsystem_vendor: 0,
        subsystem_device: 0,
        class,
        subclass: 0,
        prog_if: 0,
        revision: 0,
        resources,
    }
}

/// The boot enumeration finds the platform ATA seed and the QEMU PCI set,
/// with no phantom (0xFFFF) functions and a bridge present on every machine
/// LazyOS boots (q35 and the default `i440fx`).
pub fn pci_enumeration() -> Result<(), String> {
    crate::dev::init();
    let table = crate::dev::table().lock();
    let devices: Vec<DeviceInfo> = table.iter().collect();
    check!(!devices.is_empty(), "device table is empty");
    for (index, info) in devices.iter().enumerate() {
        check!(
            info.id.0 as usize == index,
            "device id {} is not its slot {index}",
            info.id.0
        );
    }
    check!(
        devices
            .iter()
            .any(|info| matches!(info.bus, BusId::Platform) && info.class == 0x01),
        "the platform ATA seed is missing"
    );
    let pci_devices: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|info| matches!(info.bus, BusId::Pci(_)))
        .collect();
    check!(!pci_devices.is_empty(), "no PCI devices enumerated");
    check!(
        pci_devices.iter().all(|info| info.vendor != 0xFFFF),
        "a PCI entry has the absent-vendor id 0xFFFF"
    );
    check!(
        pci_devices.iter().any(|info| info.class == 0x06),
        "no PCI bridge found"
    );
    Ok(())
}

/// Decodes a size from a write-ones mask, and probes a live BAR: the length is
/// a power of two and the original BAR value is restored afterwards.
pub fn pci_bar_size_probe() -> Result<(), String> {
    // The pure decode arithmetic first (no hardware needed).
    check!(
        pci::decode_size(0xFFFF_FFF0, false, false) == Some(0x10),
        "memory mask 0xFFFF_FFF0 decoded to {:?}",
        pci::decode_size(0xFFFF_FFF0, false, false)
    );
    check!(
        pci::decode_size(0xFF00_0000, false, false) == Some(0x0100_0000),
        "16 MiB mask decoded wrong"
    );
    check!(
        pci::decode_size(0, false, false).is_none(),
        "an all-implemented mask decoded to a window"
    );
    check!(
        pci::decode_size(0xFFFF_FFFC, true, false) == Some(4),
        "I/O mask 0xFFFF_FFFC decoded wrong"
    );
    check!(
        pci::decode_size(0xFFFF_FFFF_FFFF_FF00, false, true) == Some(0x100),
        "64-bit mask decoded wrong"
    );

    // Live probe over the enumerated QEMU devices.
    let mut outcome: Option<String> = None;
    pci::for_each(|function| {
        if outcome.is_some() {
            return;
        }
        let address = function.address;
        for index in 0..6u8 {
            let before = pci::bar_raw(address, index);
            if before == 0 {
                continue;
            }
            let Some((bar, _stride)) = pci::read_bar(address, index) else {
                continue;
            };
            if bar.len == 0 || !bar.len.is_power_of_two() {
                outcome = Some(format!(
                    "device {:04x}:{:04x} BAR {index} has length {}",
                    function.vendor, function.id, bar.len
                ));
                return;
            }
            if pci::bar_raw(address, index) != before {
                outcome = Some(format!(
                    "device {:04x}:{:04x} BAR {index} was not restored",
                    function.vendor, function.id
                ));
                return;
            }
            outcome = Some(String::from("ok"));
            return;
        }
    });
    match outcome.as_deref() {
        Some("ok") => Ok(()),
        Some(other) => Err(String::from(other)),
        None => Err(String::from("no BAR could be probed")),
    }
}

/// The capability-list walk terminates, respects its bound, and only reports
/// 4-byte-aligned offsets inside config space.
pub fn pci_capability_walk() -> Result<(), String> {
    let mut checked = 0usize;
    let mut error: Option<String> = None;
    pci::for_each(|function| {
        if error.is_some() {
            return;
        }
        let mut offsets = Vec::new();
        let count = pci::for_each_capability(function.address, |capability| {
            offsets.push(capability.offset);
        });
        if count != offsets.len() || count > 48 {
            error = Some(format!("capability count {count} disagrees with the walk"));
            return;
        }
        for offset in offsets {
            if !(0x40..=0xFC).contains(&offset) || offset & 0x3 != 0 {
                error = Some(format!("capability offset {offset:#x} is out of range"));
                return;
            }
        }
        checked += 1;
    });
    match error {
        Some(message) => Err(message),
        None => {
            check!(checked > 0, "no PCI function was walked");
            Ok(())
        }
    }
}

/// Claim/unclaim, double-claim refusal, and an independent second device.
pub fn claim_unclaim_double() -> Result<(), String> {
    let mut table = DeviceTable::new();
    let id = table
        .insert(test_device(0x02))
        .map_err(|error| format!("insert failed: {error:?}"))?;
    check!(
        table.len() == 1 && table.owned() == 0,
        "a fresh table is not empty/unowned"
    );

    let handle = table
        .claim(id, TaskSlot(1))
        .map_err(|error| format!("claim failed: {error:?}"))?;
    check!(
        table.owner(id) == Some(TaskSlot(1)),
        "the owner was not recorded"
    );
    check!(
        table.claim(id, TaskSlot(2)).err() == Some(DevError::Busy),
        "a double-claim was accepted"
    );
    check!(
        table.owner(id) == Some(TaskSlot(1)),
        "the refused double-claim changed the owner"
    );

    table
        .release(handle)
        .map_err(|error| format!("release failed: {error:?}"))?;
    check!(table.owner(id).is_none(), "the owner survived release");
    check!(table.owned() == 0, "the owned count survived release");

    let again = table
        .claim(id, TaskSlot(2))
        .map_err(|error| format!("re-claim failed: {error:?}"))?;
    check!(
        again.generation() == handle.generation() + 1,
        "re-claim did not advance the generation"
    );
    table
        .release(again)
        .map_err(|error| format!("second release failed: {error:?}"))?;

    check!(
        table.claim(DeviceId(99), TaskSlot(1)).err() == Some(DevError::NotFound),
        "claiming an unknown id did not fail NotFound"
    );
    Ok(())
}

/// A handle from a released claim must never act on the slot again.
pub fn stale_generation_rejected() -> Result<(), String> {
    let mut table = DeviceTable::new();
    let id = table
        .insert(test_device(0x02))
        .map_err(|error| format!("insert failed: {error:?}"))?;
    let first = table
        .claim(id, TaskSlot(1))
        .map_err(|error| format!("claim failed: {error:?}"))?;
    table
        .release(first)
        .map_err(|error| format!("release failed: {error:?}"))?;

    check!(
        table.release(first).err() == Some(DevError::Stale),
        "a released handle was accepted a second time"
    );

    let second = table
        .claim(id, TaskSlot(2))
        .map_err(|error| format!("re-claim failed: {error:?}"))?;
    check!(
        second.generation() == first.generation() + 1,
        "the new claim reused the old generation"
    );
    check!(
        table.release(first).err() == Some(DevError::Stale),
        "a stale handle released the new owner"
    );
    check!(
        table.owner(id) == Some(TaskSlot(2)),
        "the stale release changed the live owner"
    );
    table
        .release(second)
        .map_err(|error| format!("live release failed: {error:?}"))?;
    Ok(())
}

/// One million claim/release cycles on a single slot must not leak an owner, a
/// slot, or a generation, and every older handle must stay stale afterwards.
pub fn claim_release_soak() -> Result<(), String> {
    const CYCLES: u32 = 1_000_000;
    let mut table = DeviceTable::new();
    let id = table
        .insert(test_device(0x02))
        .map_err(|error| format!("insert failed: {error:?}"))?;
    for cycle in 0..CYCLES {
        let owner = TaskSlot((cycle % 3) as usize + 1);
        let handle = table
            .claim(id, owner)
            .map_err(|error| format!("claim {cycle} failed: {error:?}"))?;
        table
            .release(handle)
            .map_err(|error| format!("release {cycle} failed: {error:?}"))?;
    }
    check!(table.owned() == 0, "an owner leaked across the soak");
    check!(table.owner(id).is_none(), "the slot stayed owned");
    check!(table.len() == 1, "the table gained or lost entries");
    check!(
        table.generation(id) == Some(CYCLES),
        "generation is {:?}, expected {CYCLES}",
        table.generation(id)
    );
    // A handle minted at generation 0 must now be stale.
    let stale = DeviceHandle::legacy_for_test(id, 0);
    check!(
        table.release(stale).err() == Some(DevError::Stale),
        "an ancient handle was accepted after the soak"
    );
    Ok(())
}

/// A mock driver that accepts one class and either attaches or fails.
struct MockDriver {
    class: u8,
    fail: bool,
}

impl Driver for MockDriver {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        info.class == self.class
    }

    fn attach(&self, _handle: DeviceHandle) -> Result<(), DevError> {
        if self.fail {
            Err(DevError::Io)
        } else {
            Ok(())
        }
    }
}

/// A failed `attach` must roll its claim back (device stays claimable), must
/// not disturb a device another driver attached, and must not deadlock on the
/// table lock. This is the path a boot with no ATA disk takes.
pub fn attach_failure_rolls_back() -> Result<(), String> {
    let table = Mutex::new(DeviceTable::new());
    let (bad, good) = {
        let mut guard = table.lock();
        let bad = guard
            .insert(test_device(0x01))
            .map_err(|e| format!("{e:?}"))?;
        let good = guard
            .insert(test_device(0x02))
            .map_err(|e| format!("{e:?}"))?;
        (bad, good)
    };
    let drivers: [&dyn Driver; 2] = [
        &MockDriver {
            class: 0x01,
            fail: true,
        },
        &MockDriver {
            class: 0x02,
            fail: false,
        },
    ];
    check!(
        attach_all(&table, &drivers) == 1,
        "expected exactly the good device to attach"
    );
    let guard = table.lock();
    check!(
        guard.owner(bad).is_none(),
        "a failed attach left the device claimed"
    );
    check!(
        guard.owner(good) == Some(TaskSlot::KERNEL),
        "the successful attach was not claimed for the kernel"
    );
    Ok(())
}

/// Stress: the failed-attach rollback repeated many times leaks nothing and
/// leaves the device claimable every time.
pub fn attach_failure_soak() -> Result<(), String> {
    const ROUNDS: u32 = 100_000;
    let table = Mutex::new(DeviceTable::new());
    let id = table
        .lock()
        .insert(test_device(0x01))
        .map_err(|e| format!("{e:?}"))?;
    let drivers: [&dyn Driver; 1] = [&MockDriver {
        class: 0x01,
        fail: true,
    }];
    for round in 0..ROUNDS {
        check!(attach_all(&table, &drivers) == 0, "round {round} attached");
        check!(
            table.lock().owner(id).is_none(),
            "round {round} leaked a claim"
        );
    }
    check!(
        table.lock().generation(id) == Some(ROUNDS),
        "each rollback should bump the generation exactly once"
    );
    Ok(())
}

/// I/O BARs that implement only 16 address bits (upper half reads zero) still
/// size correctly, and a full 32-bit mask is unchanged.
pub fn pci_io_bar_16bit_mask() -> Result<(), String> {
    check!(
        pci::decode_size(u64::from(pci::io_mask(0x0000_FFC1)), true, false) == Some(0x40),
        "16-bit I/O mask decoded wrong"
    );
    check!(
        pci::decode_size(u64::from(pci::io_mask(0xFFFF_FFC1)), true, false) == Some(0x40),
        "32-bit I/O mask decoded wrong"
    );
    Ok(())
}

/// BAR sizing must leave every function's command register exactly as it found
/// it (decode is switched off during sizing and restored).
pub fn pci_sizing_restores_command() -> Result<(), String> {
    let mut error: Option<String> = None;
    pci::for_each(|function| {
        let address = function.address;
        let before = pci::command(address);
        for index in 0..6u8 {
            let _ = pci::read_bar(address, index);
        }
        let after = pci::command(address);
        if before != after && error.is_none() {
            error = Some(format!(
                "{:04x}:{:04x} command changed {before:#x} -> {after:#x}",
                function.vendor, function.id
            ));
        }
    });
    match error {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_pci_enumeration", pci_enumeration),
    ("dev_pci_bar_size_probe", pci_bar_size_probe),
    ("dev_pci_capability_walk", pci_capability_walk),
    ("dev_claim_unclaim_double", claim_unclaim_double),
    ("dev_stale_generation_rejected", stale_generation_rejected),
    ("dev_claim_release_soak", claim_release_soak),
    ("dev_attach_failure_rolls_back", attach_failure_rolls_back),
    ("dev_attach_failure_soak", attach_failure_soak),
    ("dev_pci_io_bar_16bit_mask", pci_io_bar_16bit_mask),
    (
        "dev_pci_sizing_restores_command",
        pci_sizing_restores_command,
    ),
];

//! Shared scaffolding of the `dev::peek` tests (`sys_peek`, `sys_peek_hw`):
//! the two ops as functions, the policy guard and a synthetic function with
//! chosen ids.

use alloc::vec;
use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::peek;
use crate::dev::syscall::{OP_DBG_CONFIG_READ, OP_DBG_MMIO_READ};
use crate::ipc::credentials::Cred;
use pciprobe::{Policy, Range};

pub(super) const DBGD: u32 = 914;
pub(super) const FILL: u8 = 0xA5;
/// A bare address no machine decodes besides HPET: reads return the
/// controller's registers or all-ones, never fault.
pub(super) const WINDOW: u64 = 0xFED0_0000;
/// The synthetic function's BDF: bus 0xFE, device 0, function 0.
pub(super) const GHOST_BDF: u64 = 0xFE00;

/// Installs a policy and puts it back to "off" on every exit path.
pub(super) struct Peek;

impl Peek {
    /// For synthetic functions: all-ones config space counts as present.
    pub(super) fn with(policy: Policy) -> Peek {
        peek::set_policy_for_test(policy);
        peek::assume_present_for_test(true);
        Peek
    }

    /// For real functions (and the gone-function case): nothing assumed.
    pub(super) fn real(policy: Policy) -> Peek {
        peek::set_policy_for_test(policy);
        peek::assume_present_for_test(false);
        Peek
    }
}

impl Drop for Peek {
    fn drop(&mut self) {
        peek::reset_for_test();
    }
}

pub(super) fn dbgd() -> Cred {
    Cred::new(DBGD, DBGD, 0, 0, 1)
}

pub(super) fn range(vendor: u16, device: u16, bar: u8, start: u64, end: u64) -> Range {
    Range {
        vendor,
        device,
        bar,
        start,
        end,
    }
}

/// A refused read: the `-errno`.
#[derive(Debug)]
pub(super) struct Fail(pub i64);

impl From<Fail> for String {
    fn from(fail: Fail) -> String {
        format!("the read failed with {}", fail.0)
    }
}

fn checked(got: i64, len: u64, buf: &mut Vec<u8>, what: &str) -> Result<Vec<u8>, Fail> {
    if got < 0 {
        return Err(Fail(got));
    }
    assert_eq!(got as u64, len, "{what}: the length returned");
    assert!(
        buf[len as usize..].iter().all(|b| *b == FILL),
        "{what} wrote past its length"
    );
    buf.truncate(len as usize);
    Ok(core::mem::take(buf))
}

/// `config_read` into a fresh buffer: the bytes or the `-errno`.
pub(super) fn config(bdf: u64, offset: u64, len: u64) -> Result<Vec<u8>, Fail> {
    let mut buf = vec![FILL; len.max(1) as usize + 8];
    let got = sys(
        OP_DBG_CONFIG_READ,
        bdf,
        offset,
        len,
        buf.as_mut_ptr() as u64,
    );
    checked(got, len, &mut buf, "config_read")
}

/// `mmio_read` into a fresh buffer.
pub(super) fn mmio(bdf: u64, bar: u64, offset: u64, len: u64) -> Result<Vec<u8>, Fail> {
    let mut buf = vec![FILL; len.max(1) as usize + 8];
    let got = sys(
        OP_DBG_MMIO_READ,
        bdf,
        offset,
        bar | len << 8,
        buf.as_mut_ptr() as u64,
    );
    checked(got, len, &mut buf, "mmio_read")
}

pub(super) fn expect_err<T: core::fmt::Debug>(
    got: Result<T, Fail>,
    errno: i64,
    what: &str,
) -> Result<(), String> {
    check!(
        matches!(&got, Err(Fail(e)) if *e == -errno),
        "{what}: {got:?}, expected -{errno}"
    );
    Ok(())
}

/// A synthetic function with chosen ids and BARs, appended like the others
/// and removed by the fixture.
pub(super) fn add_with_ids(vendor: u16, device: u16, bars: Vec<Bar>) -> Result<DeviceId, String> {
    let mut resources = Resources::empty();
    for bar in bars {
        resources.set_bar(bar);
    }
    let info = DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Pci(GHOST),
        vendor,
        device,
        subsystem_vendor: 0,
        subsystem_device: 0,
        class: 0x03,
        subclass: 0,
        prog_if: 0,
        revision: 0,
        resources,
    };
    crate::dev::table()
        .lock()
        .insert(info)
        .map_err(|error| format!("cannot add a synthetic device: {error:?}"))
}

/// The BDF word the ops take for a real function's table row.
pub(super) fn bdf_of(info: &DeviceInfo) -> Option<u64> {
    match info.bus {
        BusId::Pci(a) => {
            Some(u64::from(a.bus) << 8 | u64::from(a.device) << 3 | u64::from(a.function))
        }
        BusId::Platform => None,
    }
}

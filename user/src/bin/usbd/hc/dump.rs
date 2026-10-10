//! The controller snapshot `dbgd`'s `usb.dump` serves (see `usbd.rs`'s
//! `Dump`): registers and ports as `USBD:DUMP:*` lines. A child of `hc` so
//! it reads the controller's private state.

use alloc::vec;
use alloc::vec::Vec;

use xhci::regs::{op, portsc};

use super::Hc;

impl Hc {
    /// The controller's registers and every port, as `USBD:DUMP:*` lines
    /// (`bus::Controller::dump`): what a person with a register dump and
    /// the xHCI spec needs to see why a device does not enumerate.
    pub(crate) fn dump_lines(&self) -> Vec<alloc::string::String> {
        let wide =
            |offset: usize| u64::from(self.opreg(offset)) | u64::from(self.opreg(offset + 4)) << 32;
        let mut lines = vec![alloc::format!(
            "USBD:DUMP:HC hc={} version={:#x} ports={} slots={} scratchpads={} csz64={} usbcmd={:#x} usbsts={:#x} crcr={:#x} dcbaap={:#x} config={:#x} cmd_dq={:#x} erdp={:#x} pending={} dropped={}",
            self.index,
            self.info.version,
            self.info.ports,
            self.info.slots,
            self.info.scratchpads,
            self.info.context_64,
            self.opreg(op::USBCMD),
            self.opreg(op::USBSTS),
            wide(op::CRCR),
            wide(op::DCBAAP),
            self.opreg(op::CONFIG),
            self.commands.dequeue_pointer(),
            self.events.erdp(),
            self.pending.len(),
            self.dropped,
        )];
        for port in 1..=self.info.ports {
            let sc = self.portsc(port);
            lines.push(alloc::format!(
                "USBD:DUMP:PORT hc={} port={port} portsc={sc:#x} ccs={} ped={} pr={} pls={}",
                self.index,
                sc & portsc::CCS != 0,
                sc & portsc::PED != 0,
                sc & portsc::PR != 0,
                (sc & portsc::PLS_MASK) >> portsc::PLS_SHIFT,
            ));
        }
        lines
    }
}

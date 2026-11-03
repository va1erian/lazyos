//! Controller bring-up glue: drive `bthci::Init`, size the ACL buffers once
//! the controller says how many it has, and mark the moments the boot path is
//! measured by (reset done, `BT:READY`, `CTRL:PASS`). Split out of `host.rs`
//! (file-length limit).

use alloc::format;

use bthci::{AclBuffers, Event, Input, Phase};

use crate::host::Host;

impl Host {
    pub(crate) fn init_step(&mut self, now: u64, input: Input<'_>) {
        // Boot-latency span (docs/bluetooth-plan.md B6): `HCI_Reset` is done.
        if let Input::Event(Event::CommandComplete { opcode, .. }) = input {
            if *opcode == bthci::opcode::RESET {
                self.note(format!("SPAN step=reset-done ms={now}"));
            }
        }
        let was_armed = self.init.is_armed();
        let step = self.init.step(now, input);
        self.init_deadline = step.deadline_ms;
        if let Some(command) = step.send {
            self.cmd(command);
        }
        if self.bufs.is_none() && self.init.info().acl.acl_len != 0 {
            let size = self.init.info().acl;
            self.bufs = Some(AclBuffers::new(size.acl_len, size.acl_num));
        }
        if !was_armed && self.init.is_armed() {
            self.on_armed(now);
        }
        if self.init.info().bd_addr.is_some() {
            self.le_address_ready(now);
        }
        match self.init.phase() {
            Phase::Complete if !self.ctrl_reported => {
                self.ctrl_reported = true;
                self.fw_reenumerations = 0;
                self.report_controller();
                self.send_harness_passkey();
            }
            Phase::Failed(error) => self.fail(format!("bring-up failed: {error:?}")),
            _ => {}
        }
    }

    /// The fast path is done: say so, open the pairing window and start the
    /// pages that wait for the host (plan decision 5).
    pub(crate) fn on_armed(&mut self, now: u64) {
        self.armed_at = Some(now);
        self.note(format!("READY ms={now}"));
        self.open_pairing_window(now);
        self.pager_arm(now);
        self.le_armed(now);
    }

    pub(crate) fn report_controller(&mut self) {
        let info = self.init.info().clone();
        let (hci, lmp, vendor) = info.version.map_or((0, 0, 0), |v| {
            (v.hci_version, v.lmp_version, v.manufacturer)
        });
        let addr = info
            .bd_addr
            .map_or(alloc::string::String::from("unknown"), |a| {
                let b = a.to_msb();
                format!("**:**:**:**:{:02X}:{:02X}", b[4], b[5])
            });
        self.note(format!(
            "CTRL:PASS:hci={hci} lmp={lmp} vendor={vendor:#06x} addr={addr}"
        ));
    }
}

//! The hub class (USB 2.0 11, USB 3.2 10): keyboards sit behind monitor
//! hubs, keyboards with a hub of their own, and on-board hubs that some
//! boards route front-panel ports through.
//!
//! A hub is configured like any device, then told it is a hub (the slot
//! context's Hub, Number of Ports, TT Think Time and MTT fields, given with
//! its status-change endpoint in one Configure Endpoint). Its ports are
//! powered, and every port its status-change bitmap flags is examined with
//! GET_STATUS: a disconnect detaches what hung there, a connect is
//! debounced, reset and recovered with the same timings as a root port, and
//! the device below is enumerated at its own route string. The controller
//! module (`bus.rs`) does the attaching; this module only speaks to the hub.
//!
//! Bounded: at most [`hub::MAX_PORTS`] ports per hub (all a route string can
//! name) and five tiers of hubs (`xhci::route`).

use alloc::collections::VecDeque;
use alloc::format;

use usbhid::desc::{Config, Transfer, CLASS_HUB};
use usbhid::hub::{self, feature, PortSpeed, PortStatus};
use user::sys;
use xhci::context::HubSlot;
use xhci::regs::Speed;
use xhci::trb::request;

use super::device::Device;
use super::hc::{nap, sleep_ms, Hc};
use super::port::{DEBOUNCE_MS, RESET_RECOVERY_MS};
use super::Error;

/// How long a hub port reset may take before the port is given up.
const RESET_TICKS: u64 = 50;
/// The shortest wait after powering a hub's ports, whatever the hub says.
const MIN_POWER_ON_MS: u64 = 100;
/// `bDeviceProtocol` of a high-speed hub with one TT per port.
const PROTOCOL_MULTI_TT: u8 = 2;

/// A configured hub.
pub(super) struct HubFn {
    pub(super) dci: u8,
    pub(super) ports: u8,
    superspeed: bool,
    pub(super) multi_tt: bool,
    power_on_ms: u64,
    /// Ports flagged by the status-change bitmap and not yet examined.
    pub(super) pending: VecDeque<u8>,
}

/// What GET_STATUS said about a port.
pub(super) struct PortState {
    pub(super) connected: bool,
    /// The connection changed since it was last looked at (a replug).
    pub(super) changed: bool,
}

impl HubFn {
    /// Prepare the hub on `device`: its depth (SuperSpeed), multiple TTs
    /// (when it has them), its descriptor and its status-change pipe.
    /// Returns what its slot context must declare.
    pub(super) fn bind(
        hc: &mut Hc,
        device: &mut Device,
        config: &Config,
    ) -> Result<(HubFn, HubSlot), Error> {
        let interface = config
            .interfaces()
            .find(|i| i.class == CLASS_HUB && i.alternate == 0)
            .ok_or(Error::Descriptor("hub without a hub interface"))?;
        let endpoint = interface
            .endpoint(Transfer::Interrupt, true)
            .ok_or(Error::Descriptor("hub without a status-change endpoint"))?;
        let superspeed = matches!(device.at.speed, Speed::Super | Speed::SuperPlus);
        if superspeed {
            device.control_out(hc, request::set_hub_depth(device.at.depth))?;
        }
        let mut multi_tt = false;
        let has_mtt_setting = config.interfaces().any(|i| {
            i.number == interface.number && i.alternate == 1 && i.protocol == PROTOCOL_MULTI_TT
        });
        if device.at.speed == Speed::High
            && device.descriptor.protocol == PROTOCOL_MULTI_TT
            && has_mtt_setting
        {
            multi_tt = device
                .control_out(hc, request::set_interface(interface.number, 1))
                .is_ok();
        }
        let kind = if superspeed {
            request::SS_HUB_DESCRIPTOR
        } else {
            request::HUB_DESCRIPTOR
        };
        // 12 bytes hold a SuperSpeed descriptor and a USB 2 one for up to
        // 15 ports (7 bytes and two port bitmaps); a shorter answer is fine,
        // its own bLength says how much of the buffer is real.
        let mut bytes = [0u8; 12];
        device.control_in(hc, request::get_hub_descriptor(kind, 12), &mut bytes)?;
        let descriptor =
            hub::parse_hub(&bytes, superspeed).map_err(|_| Error::Descriptor("hub descriptor"))?;
        let ports = descriptor.ports.min(hub::MAX_PORTS);
        let dci = device.open_reports(&endpoint, 1)?;
        let slot = HubSlot {
            ports,
            multi_tt,
            think_time: if device.at.speed == Speed::High {
                descriptor.think_time
            } else {
                0
            },
        };
        sys::write_str(&format!(
            "USBD:HUB port={} slot={} ports={} reported={} speed={:?} mtt={} depth={}\n",
            device.name,
            device.slot,
            ports,
            descriptor.ports,
            device.at.speed,
            multi_tt,
            device.at.depth
        ));
        let hub = HubFn {
            dci,
            ports,
            superspeed,
            multi_tt,
            power_on_ms: u64::from(descriptor.power_on_ms).max(MIN_POWER_ON_MS),
            pending: VecDeque::new(),
        };
        Ok((hub, slot))
    }

    /// After Configure Endpoint: power every port and queue them all for a
    /// first look (a device already plugged in raises no change by itself
    /// on every hub).
    pub(super) fn start(&mut self, hc: &mut Hc, device: &mut Device) -> Result<(), Error> {
        for port in 1..=self.ports {
            device.control_out(hc, request::set_port_feature(port, feature::PORT_POWER))?;
        }
        sleep_ms(self.power_on_ms);
        self.pending.extend(1..=self.ports);
        Ok(())
    }

    /// A status-change bitmap arrived: queue the ports it flags.
    pub(super) fn on_report(&mut self, bitmap: &[u8]) {
        for port in hub::changed_ports(bitmap, self.ports) {
            if !self.pending.contains(&port) {
                self.pending.push_back(port);
            }
        }
    }

    fn status(&self, hc: &mut Hc, device: &mut Device, port: u8) -> Result<PortStatus, Error> {
        let mut bytes = [0u8; 4];
        device.control_in(hc, request::get_port_status(port), &mut bytes)?;
        PortStatus::decode(&bytes, self.superspeed).map_err(|_| Error::Descriptor("port status"))
    }

    /// Read `port`'s status and acknowledge every change bit.
    pub(super) fn check(
        &self,
        hc: &mut Hc,
        device: &mut Device,
        port: u8,
    ) -> Result<PortState, Error> {
        let status = self.status(hc, device, port)?;
        for selector in status.change_features() {
            device.control_out(hc, request::clear_port_feature(port, selector))?;
        }
        Ok(PortState {
            connected: status.connected(),
            changed: status.connect_changed(),
        })
    }

    /// Debounce, reset and recover a connected port; the speed of the
    /// device on it, `None` when it went away or would not enable.
    pub(super) fn enable(
        &self,
        hc: &mut Hc,
        device: &mut Device,
        port: u8,
    ) -> Result<Option<Speed>, Error> {
        sleep_ms(DEBOUNCE_MS);
        let status = self.status(hc, device, port)?;
        if !status.connected() {
            return Ok(None);
        }
        // A SuperSpeed port trains on its own; it needs a warm reset only
        // when the link is stuck. A USB 2 port always gets a reset.
        if !(self.superspeed && status.enabled()) {
            let reset = if self.superspeed {
                feature::BH_PORT_RESET
            } else {
                feature::PORT_RESET
            };
            device.control_out(hc, request::set_port_feature(port, reset))?;
            let deadline = sys::clock() + RESET_TICKS;
            loop {
                let now = self.status(hc, device, port)?;
                if now.reset_changed() && !now.resetting() {
                    break;
                }
                if sys::clock() > deadline {
                    return Ok(None);
                }
                nap();
            }
        }
        let status = self.status(hc, device, port)?;
        for selector in status.change_features() {
            device.control_out(hc, request::clear_port_feature(port, selector))?;
        }
        if !(status.connected() && status.enabled()) {
            return Ok(None);
        }
        sleep_ms(RESET_RECOVERY_MS);
        Ok(Some(match status.speed() {
            PortSpeed::Low => Speed::Low,
            PortSpeed::Full => Speed::Full,
            PortSpeed::High => Speed::High,
            PortSpeed::Super => Speed::Super,
        }))
    }
}
